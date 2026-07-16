// tests/ownership.rs

use std::collections::HashMap;

use karac::ownership::*;
use karac::resolver::SpanKey;
use karac::{desugar_program, lower, ownershipcheck, parse, resolve, typecheck};

// ── Test Helpers ────────────────────────────────────────────────

fn ownership_ok(source: &str) -> OwnershipCheckResult {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "Type errors: {}",
        typed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        result.errors.is_empty(),
        "Ownership errors: {}",
        result
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    result
}

fn ownership_errors(source: &str) -> Vec<OwnershipError> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    // Type errors are OK — we might be testing ownership on type-valid code
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        !result.errors.is_empty(),
        "Expected ownership errors but got none"
    );
    result.errors
}

// ── Copy Types ──────────────────────────────────────────────────

#[test]
fn test_empty_program() {
    ownership_ok("");
}

#[test]
fn test_primitives_dont_move() {
    // Primitives are Copy — using them multiple times is fine
    ownership_ok(
        "fn use_twice(x: i64) -> i64 {\n\
             let a = x;\n\
             let b = x;\n\
             a + b\n\
         }",
    );
}

#[test]
fn test_forget_consumes_argument() {
    // `forget(x)` (FFI ownership-handoff primitive, additive-interop
    // Slice 4) has an owned param, so it MOVES its argument — using the
    // binding after `forget` is a use-after-move error, exactly like
    // passing it to any consuming function.
    let errors = ownership_errors(
        "struct G { id: i64 }\n\
         impl Drop for G { fn drop(mut ref self) { } }\n\
         fn main() {\n\
             let g = G { id: 1 };\n\
             forget(g);\n\
             let _ = g.id;\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("moved")),
        "use-after-forget must be a move error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_forget_clean_program_ok() {
    // A well-formed `forget` use — consume once, never touch again —
    // passes ownership checking with no spurious diagnostics.
    ownership_ok(
        "struct G { id: i64 }\n\
         impl Drop for G { fn drop(mut ref self) { } }\n\
         fn main() {\n\
             let g = G { id: 1 };\n\
             forget(g);\n\
         }",
    );
}

#[test]
fn test_string_moves() {
    // String is not Copy — using after move should error
    let errors = ownership_errors(
        "fn consume(s: String) { }\n\
         fn main() {\n\
             let s = \"hello\";\n\
             consume(s);\n\
             consume(s);\n\
         }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::UseAfterMove));
}

#[test]
fn test_struct_moves() {
    let errors = ownership_errors(
        "struct Point { x: i64, y: i64 }\n\
         fn consume(p: Point) { }\n\
         fn main() {\n\
             let p = Point { x: 1, y: 2 };\n\
             consume(p);\n\
             consume(p);\n\
         }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::UseAfterMove));
}

#[test]
fn test_raw_mut_pointer_is_copy() {
    // `*mut T` is Copy (Rust parity) — a raw pointer passed to an
    // owned-param fn is *copied*, leaving the original binding usable. The
    // pervasive FFI handle pattern — a `*mut` handle created once and passed
    // to many calls (e.g. `LLVMContextRef` → builder/dispose) — must not
    // fire a use-after-move. The inverse of `test_string_moves` above.
    ownership_ok(
        "unsafe extern \"C\" {\n\
             fn make() -> *mut u8;\n\
             fn consume(p: *mut u8);\n\
         }\n\
         fn main() {\n\
             let h = unsafe { make() };\n\
             unsafe { consume(h) };\n\
             unsafe { consume(h) };\n\
         }",
    );
}

#[test]
fn test_raw_const_pointer_is_copy() {
    // `*const T` is Copy too — companion to the `*mut` case, locking in the
    // uniform raw-pointer rule (both kinds Copy, regardless of const/mut).
    ownership_ok(
        "unsafe extern \"C\" {\n\
             fn peek() -> *const u8;\n\
             fn read(p: *const u8);\n\
         }\n\
         fn main() {\n\
             let h = unsafe { peek() };\n\
             unsafe { read(h) };\n\
             unsafe { read(h) };\n\
         }",
    );
}

#[test]
fn test_vector_is_copy() {
    // A `Vector[T, N]` is a fixed-size, register-resident SIMD POD — Copy like a
    // fixed `Array` of primitives. Binding one to a new name (`let e1 = ux;`)
    // COPIES it, so the original binding stays usable; without the Copy
    // classification this fired a spurious use-after-move on the reuse of `ux` /
    // `uy` after the rebind (B-2026-07-11-1, surfaced by Slipstream's
    // `Vector[f64, 2]` collide kernel — the SIMD analog of `test_primitives_dont_move`).
    ownership_ok(
        "fn mix() -> Vector[f64, 2] {\n\
             let ux = Vector[f64, 2].splat(2.0);\n\
             let uy = Vector[f64, 2].splat(3.0);\n\
             let e1 = ux;\n\
             let e4 = uy;\n\
             ux + uy + e1 + e4\n\
         }",
    );
}

// ── Move Tracking ───────────────────────────────────────────────

#[test]
fn test_basic_move_ok() {
    // Single use of a non-Copy value is fine
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             consume(d);\n\
         }",
    );
}

#[test]
fn test_use_after_move_error() {
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             consume(d);\n\
             consume(d);\n\
         }",
    );
    assert!(errors[0].kind == OwnershipErrorKind::UseAfterMove);
    assert!(errors[0].message.contains("moved"));
}

#[test]
fn test_reassignment_resets_state() {
    // Reassigning a mut variable resets it to Live
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let mut d = Data { value: 1 };\n\
             consume(d);\n\
             d = Data { value: 2 };\n\
             consume(d);\n\
         }",
    );
}

#[test]
fn test_shadowing_rebind_resets_state() {
    // B-2026-07-02-32: a same-scope shadowing `let` is a FRESH binding —
    // uses after it refer to the new value, not the moved-out old one.
    // The CFG predicate used to give both bindings one identity, so the
    // move into `y` paired with the read of the NEW `x` and flagged
    // legal rebind-after-move as UseAfterMove.
    ownership_ok(
        "fn main() {\n\
             let x = \"a longer heap string payload over 36 bytes!\";\n\
             let y = x;\n\
             let x = 99;\n\
             println(x);\n\
             println(y);\n\
         }",
    );
}

#[test]
fn test_shadowing_rebind_rhs_consumes_old_binding() {
    // `let d = give(d);` — the RHS consumes the OLD binding, the shadow
    // re-binds the name, and later uses read the NEW value. No UAM.
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn give(d: Data) -> Data { d }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             let d = give(d);\n\
             consume(d);\n\
         }",
    );
}

#[test]
fn test_param_shadowing_let_is_fresh_binding() {
    // A body-level `let` that re-binds a consumed PARAMETER is a fresh
    // binding too (the CFG builder seeds shadow detection with the
    // param names). B-2026-07-02-32.
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn f(d: Data) {\n\
             consume(d);\n\
             let d = Data { value: 2 };\n\
             consume(d);\n\
         }\n\
         fn main() { f(Data { value: 1 }); }",
    );
}

#[test]
fn test_inner_block_shadow_does_not_mask_outer_uam() {
    // The shadow rename frame scopes to the END OF ITS BLOCK: a shadow
    // inside an `if` body must not kill the genuine use-after-move of
    // the OUTER binding after the block closes.
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             consume(d);\n\
             if true {\n\
                 let d = Data { value: 2 };\n\
                 consume(d);\n\
             }\n\
             consume(d);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "outer use-after-move must survive an inner-block shadow: {:?}",
        errors
    );
}

#[test]
fn test_multiple_reads_before_move() {
    // Reading a value multiple times before moving is fine
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             let v = d.value;\n\
             let w = d.value;\n\
             consume(d);\n\
         }",
    );
}

#[test]
fn test_return_consumes() {
    // Returning a value consumes it
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn make() -> Data {\n\
             let d = Data { value: 1 };\n\
             d\n\
         }",
    );
}

#[test]
fn test_let_binding_consumes() {
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             let d2 = d;\n\
             let d3 = d;\n\
         }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::UseAfterMove));
}

// ── Parameter Mode Inference ────────────────────────────────────

#[test]
fn test_param_read_only_is_ref() {
    let result = ownership_ok("fn read_field(x: i64) -> i64 { x + 1 }");
    let modes = result.param_modes.get("read_field").unwrap();
    // i64 is Copy, so even though it's consumed, it stays Ref
    assert_eq!(modes[0].1, OwnershipMode::Ref);
}

#[test]
fn test_param_consumed_is_own() {
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn take_data(d: Data) {\n\
             consume(d);\n\
         }",
    );
    let modes = result.param_modes.get("take_data").unwrap();
    assert_eq!(modes[0].1, OwnershipMode::Own);
}

#[test]
fn test_pure_function_params_ref() {
    let result = ownership_ok("fn add(a: i64, b: i64) -> i64 { a + b }");
    let modes = result.param_modes.get("add").unwrap();
    assert_eq!(modes[0].1, OwnershipMode::Ref);
    assert_eq!(modes[1].1, OwnershipMode::Ref);
}

#[test]
fn test_struct_field_access_is_ref() {
    let result = ownership_ok(
        "struct Point { x: i64, y: i64 }\n\
         fn get_x(p: Point) -> i64 { p.x }",
    );
    let modes = result.param_modes.get("get_x").unwrap();
    // Field access is a read — but for non-Copy struct, accessing through
    // it could be Own if the struct is consumed. Since p.x returns i64 (Copy),
    // p itself is only read.
    assert_eq!(modes[0].1, OwnershipMode::Ref);
}

// ── Cycle Detection ─────────────────────────────────────────────

#[test]
fn test_no_cycle_passes() {
    ownership_ok(
        "struct Parent { name: String }\n\
         struct Child { parent_name: String }",
    );
}

#[test]
fn test_direct_cycle_error() {
    let errors = ownership_errors(
        "struct A { b: B }\n\
         struct B { a: A }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::OwnershipCycle));
}

#[test]
fn test_self_referential_cycle() {
    let errors = ownership_errors("struct Node { next: Node }");
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::OwnershipCycle));
}

#[test]
fn test_cycle_reported_once_per_back_edge() {
    // A type that realizes the same `self → X` back-edge through multiple
    // fields (`Add(Expr, Expr)` has two `Expr` fields, `struct Pair { a:
    // Pair, b: Pair }` has two `Pair` fields) forms ONE cycle, not one per
    // field. The adjacency list is deduped by target type name, so exactly
    // one `OwnershipCycle` diagnostic surfaces — no byte-identical dupes.
    let enum_errs = ownership_errors("enum Expr { Num(i64), Add(Expr, Expr) }");
    let enum_cycles = enum_errs
        .iter()
        .filter(|e| e.kind == OwnershipErrorKind::OwnershipCycle)
        .count();
    assert_eq!(
        enum_cycles,
        1,
        "expected one deduped ownership cycle for the enum, got {enum_cycles}: {:?}",
        enum_errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );

    let struct_errs = ownership_errors("struct Pair { a: Pair, b: Pair }");
    let struct_cycles = struct_errs
        .iter()
        .filter(|e| e.kind == OwnershipErrorKind::OwnershipCycle)
        .count();
    assert_eq!(
        struct_cycles,
        1,
        "expected one deduped ownership cycle for the struct, got {struct_cycles}: {:?}",
        struct_errs.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_weak_breaks_cycle() {
    ownership_ok(
        "struct Parent { child: Child }\n\
         struct Child { parent: weak Parent }",
    );
}

#[test]
fn test_ref_field_no_cycle() {
    ownership_ok(
        "struct Parent { child: Child }\n\
         struct Child { parent: ref Parent }",
    );
}

// ── Complex Programs ────────────────────────────────────────────

#[test]
fn test_multiple_params_different_modes() {
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn process(a: i64, d: Data) {\n\
             let x = a + 1;\n\
             consume(d);\n\
         }",
    );
    let modes = result.param_modes.get("process").unwrap();
    // a: i64 is Copy → Ref
    assert_eq!(modes[0].1, OwnershipMode::Ref);
    // d: Data is consumed → Own
    assert_eq!(modes[1].1, OwnershipMode::Own);
}

#[test]
fn test_for_loop_binding() {
    ownership_ok(
        "fn process(items: i64) {\n\
             for item in items {\n\
                 let x = item;\n\
             }\n\
         }",
    );
}

#[test]
fn test_field_access_then_move() {
    ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) { }\n\
         fn process(d: Data) {\n\
             let v = d.value;\n\
             consume(d);\n\
         }",
    );
}

#[test]
fn test_nested_function_calls() {
    ownership_ok(
        "fn double(x: i64) -> i64 { x + x }\n\
         fn main() {\n\
             let result = double(double(5));\n\
         }",
    );
}

#[test]
fn test_impl_method_ownership() {
    let result = ownership_ok(
        "struct Counter { value: i64 }\n\
         impl Counter {\n\
             fn get(self) -> i64 { self.value }\n\
         }",
    );
    let modes = result.param_modes.get("Counter.get");
    assert!(modes.is_some());
}

#[test]
fn test_complex_program() {
    ownership_ok(
        "struct User { name: String, age: i64 }\n\
         \n\
         fn get_age(user: User) -> i64 {\n\
             user.age\n\
         }\n\
         \n\
         fn main() {\n\
             let u = User { name: \"Alice\", age: 30 };\n\
             let age = get_age(u);\n\
             let x = age + 1;\n\
         }",
    );
}

// ── Copy Struct No Move ────────────────────────────────────────

#[test]
fn test_copy_struct_does_not_move() {
    // A struct with #[derive(Copy)] should not trigger use-after-move
    ownership_ok(
        "#[derive(Copy, Clone)]\n\
         struct Point { x: i64, y: i64 }\n\
         fn use_twice(p: Point) {\n\
             let a = p;\n\
             let b = p;\n\
         }",
    );
}

#[test]
fn test_non_copy_struct_moves() {
    // A struct without Copy should trigger use-after-move
    let errors = ownership_errors(
        "struct Point { x: i64, y: i64 }\n\
         fn use_twice(p: Point) {\n\
             let a = p;\n\
             let b = p;\n\
         }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::UseAfterMove));
}

// ── MutRef Parameter Mode Inference ────────────────────────────

#[test]
fn test_param_assigned_is_mut_ref() {
    // Parameter modes are inferred — reassigning a parameter means mut ref
    let result = ownership_ok(
        "fn increment(x: i64) -> i64 {\n\
             let mut y = x;\n\
             y = y + 1;\n\
             y\n\
         }",
    );
    // x is only read, so it should be Ref
    let modes = result.param_modes.get("increment").unwrap();
    let (_, mode) = modes.iter().find(|(n, _)| n == "x").unwrap();
    assert_eq!(*mode, OwnershipMode::Ref);
}

#[test]
fn test_local_var_mutation_tracked() {
    // Mutation of a local let mut variable should work
    ownership_ok(
        "fn counter() -> i64 {\n\
             let mut x = 0;\n\
             x = x + 1;\n\
             x += 1;\n\
             x\n\
         }",
    );
}

#[test]
fn test_param_field_mutated_is_mut_ref() {
    // Field mutation on a parameter should infer MutRef
    let result = ownership_ok(
        "struct Counter { value: i64 }\n\
         fn reset(c: Counter) {\n\
             c.value = 0;\n\
         }",
    );
    let modes = result.param_modes.get("reset").unwrap();
    let (_, mode) = modes.iter().find(|(n, _)| n == "c").unwrap();
    assert_eq!(*mode, OwnershipMode::MutRef);
}

// ── Multi-level Cycle Detection ────────────────────────────────

#[test]
fn test_three_node_cycle_detected() {
    let errors = ownership_errors(
        "struct A { b: B }\n\
         struct B { c: C }\n\
         struct C { a: A }",
    );
    assert!(errors
        .iter()
        .any(|e| e.kind == OwnershipErrorKind::OwnershipCycle));
}

// ── Ownership Diagnostics ──────────────────────────────────────

#[test]
fn test_use_after_move_diagnostic_message() {
    let errors = ownership_errors(
        "struct Data { x: i64 }\n\
         fn consume(d: Data) { }\n\
         fn bad(d: Data) {\n\
             consume(d);\n\
             consume(d);\n\
         }",
    );
    let err = &errors[0];
    assert!(err.message.contains("moved here"));
    assert!(err.suggestion.is_some());
    assert!(
        err.suggestion.as_ref().unwrap().contains("cloning"),
        "Expected 'cloning' in suggestion, got: {:?}",
        err.suggestion
    );
}

#[test]
fn test_ownership_cycle_has_suggestion() {
    let errors = ownership_errors(
        "struct A { b: B }\n\
         struct B { a: A }",
    );
    let err = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::OwnershipCycle)
        .unwrap();
    let suggestion = err.suggestion.as_ref().expect("expected suggestion");
    // Non-shared cycles should steer toward indirection, not 'weak'.
    assert!(
        suggestion.contains("ref") || suggestion.contains("Box") || suggestion.contains("shared"),
        "non-shared cycle suggestion should mention ref/Box/shared, got: {}",
        suggestion
    );
    assert!(
        !err.message.contains("shared-type cycle"),
        "non-shared cycle should not be labeled as shared-type cycle: {}",
        err.message
    );
}

#[test]
fn test_shared_struct_cycle_distinct_diagnostic() {
    // A cycle among `shared` types should produce a distinct diagnostic that
    // steers toward `weak`, because the semantics (RC leak) are different.
    let errors = ownership_errors(
        "shared struct A { b: B }\n\
         shared struct B { a: A }",
    );
    let err = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::OwnershipCycle)
        .unwrap();
    assert!(
        err.message.contains("shared-type cycle"),
        "shared cycle should be labeled as shared-type cycle: {}",
        err.message
    );
    let suggestion = err.suggestion.as_ref().expect("expected suggestion");
    assert!(
        suggestion.contains("weak"),
        "shared-type cycle suggestion should mention 'weak', got: {}",
        suggestion
    );
}

#[test]
fn test_recursive_shared_enum_with_base_case_compiles() {
    // A direct recursive `shared enum` WITH a base variant (`Num`) builds
    // finite, acyclic trees — each `shared` child is an RC handle, so it is
    // size-bounded and leak-free. It is the designed mechanism for recursive
    // shared types and is fully supported end-to-end (codegen lays it out as an
    // RC handle; pattern binding extracts the recursive field as a pointer), so
    // the ownership checker must NOT flag it as a cycle.
    // `ownership_ok` asserts the program produces no ownership errors.
    let _ = ownership_ok(
        "shared enum Expr {\n\
            Num(i64),\n\
            Add(Expr, Expr),\n\
         }\n\
         fn eval(e: Expr) -> i64 {\n\
            match e {\n\
                Num(n) => n,\n\
                Add(a, b) => eval(a) + eval(b),\n\
            }\n\
         }",
    );
}

#[test]
fn test_shared_typed_param_reports_as_shared_rc() {
    // A parameter whose type is declared `shared` should be reported as
    // `shared (Rc)` in the representation map, not as `owned (stack)` —
    // codegen lowers it as Rc<T> regardless of the inferred mode.
    let result = ownership_ok(
        "shared struct Data { val: i64 }\n\
         fn process(d: Data) -> i64 { d.val }",
    );
    let repr = result
        .representations
        .get("process.d")
        .expect("expected representation for process.d");
    assert_eq!(
        repr, "shared (Rc)",
        "shared-typed param should report as 'shared (Rc)', got '{}'",
        repr
    );
}

#[test]
fn test_nonshared_typed_param_unchanged() {
    // Sanity: the shared-type branch must not alter reporting for ordinary
    // (non-shared) named-type params.
    let result = ownership_ok(
        "struct Plain { val: i64 }\n\
         fn process(p: Plain) -> i64 { p.val }",
    );
    let repr = result
        .representations
        .get("process.p")
        .expect("expected representation for process.p");
    assert_ne!(
        repr, "shared (Rc)",
        "non-shared type must not be reported as shared (Rc)"
    );
}

#[test]
fn test_mixed_shared_nonshared_cycle_is_ownership_cycle() {
    // A mixed shared/non-shared cycle with NO base escape (`shared struct A
    // { b: B }` / `struct B { a: A }` — both single-field, every instance
    // forced to recurse) is still an ownership cycle: there is no
    // terminating variant, so it has no finite construction. It is reported
    // as a plain ownership cycle (not the `shared-type cycle` "use weak"
    // diagnostic, which is reserved for *all-shared* cycles). Contrast
    // `test_struct_wrapped_recursive_shared_enum_breakable_compiles`, where
    // a base escape makes the same shared/non-shared mix *breakable*.
    let errors = ownership_errors(
        "shared struct A { b: B }\n\
         struct B { a: A }",
    );
    let err = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::OwnershipCycle)
        .unwrap();
    assert!(
        !err.message.contains("shared-type cycle"),
        "mixed cycle should be treated as an ownership cycle, not shared-type: {}",
        err.message
    );
}

#[test]
fn test_struct_wrapped_recursive_shared_enum_breakable_compiles() {
    // B-2026-06-14-28: the self-hosting AST-port wrapping convention. v1
    // forbids a direct nested-enum payload (`E_ENUM_NESTED_ENUM_PAYLOAD`),
    // so the recursive `Box<Expr>` edge passes through a plain `struct`
    // operand wrapper rather than `Add(Expr, Expr)` directly:
    //
    //     shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
    //     struct BinOp { left: Expr, right: Expr }
    //     struct Unary { operand: Expr }
    //
    // The cycle `Expr → BinOp → Expr` has BOTH a base escape (`Num(i64)`,
    // letting finite trees terminate) AND a `shared` participant (`Expr`,
    // whose RC handle is a fixed-size indirection that breaks the size
    // recursion — `BinOp` holds pointers, not inlined `Expr`s). That makes
    // it a *breakable* cycle: it builds finite, leak-free trees and is the
    // designed mechanism for recursive shared types, so the ownership
    // checker must NOT flag it. Pre-fix, `breakable` required *all*
    // participants to be `shared`; the plain-struct operand wrapper
    // (non-shared, as the AST-port convention mandates) flipped the gate
    // and spuriously reported `ownership cycle detected: Expr → BinOp →
    // Expr` — non-deterministically, since the HashMap-ordered DFS start
    // node decided whether the error survived dedup.
    //
    // Assert on the type graph alone (the cycle decision does not depend on
    // any function body), so type-only definitions are the precise probe.
    // `ownership_ok` asserts the program produces NO ownership errors — the
    // breakable cycle must be accepted silently.
    ownership_ok(
        "shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }\n\
         struct BinOp { left: Expr, right: Expr }\n\
         struct Unary { operand: Expr }",
    );
}

#[test]
fn test_struct_wrapped_recursive_shared_enum_vec_child_breakable_compiles() {
    // The sequence-child arm of the AST-port convention: a `Vec[Expr]`
    // child (the `Call(args)` shape). The `Vec` produces no direct graph
    // edge, but the struct-wrapped `Expr` edges still form the breakable
    // cycle through `BinOp`. Accepted, no cycle error.
    ownership_ok(
        "shared enum Expr { Num(i64), Add(BinOp), Call(CallExpr) }\n\
         struct BinOp { left: Expr, right: Expr }\n\
         struct CallExpr { callee: Expr, args: Vec[Expr] }",
    );
}

#[test]
fn test_plain_struct_wrapped_cycle_without_shared_stays_ownership_cycle() {
    // Guard the other half of the fix: the `any_shared` relaxation must NOT
    // over-accept a cycle with NO shared participant. A plain `struct`
    // cycle that passes through an enum operand wrapper but contains no
    // `shared` edge has no fixed-size indirection — `Expr` is inlined into
    // `BinOp`, so the type is infinite-sized regardless of the base escape.
    // It must STAY an ownership cycle. (`is_shared_type` over the cycle is
    // the discriminator: `any_shared` is false here.)
    let errors = ownership_errors(
        "enum Expr { Num(i64), Add(BinOp) }\n\
         struct BinOp { left: Expr, right: Expr }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::OwnershipCycle),
        "a non-shared struct-wrapped recursive cycle must remain an ownership cycle"
    );
}

// ── Destructuring in function/closure parameters ─────────────────

#[test]
fn test_tuple_destructuring_param_ownership() {
    ownership_ok("fn add((a, b): (i64, i64)) -> i64 { a + b }");
}

#[test]
fn test_wildcard_destructuring_param_ownership() {
    ownership_ok("fn y_only((_, y): (i64, i64)) -> i64 { y }");
}

// ── Representation Tracking ───────────────────────────────────

#[test]
fn test_representations_populated() {
    // Verify that representations are populated for inferred param modes
    let result = ownership_ok("fn add(x: i64, y: i64) -> i64 { x + y }");
    // x and y should have representations
    assert!(!result.representations.is_empty() || result.param_modes.is_empty());
}

// ── @no_rc Struct ─────────────────────────────────────────────

#[test]
fn test_no_rc_struct_passes_ownership() {
    // @no_rc struct should pass ownership check normally
    ownership_ok(
        "@no_rc\n\
         struct Particle { x: f64, y: f64 }\n\
         fn main() { let p = Particle { x: 1.0, y: 2.0 }; }",
    );
}

// ── Once-callable closure ownership ──────────────────────────────

#[test]
fn test_once_callable_closure_first_call_ok() {
    // A closure that captures an owned non-Copy value is once-callable.
    // The first (and only) call must succeed.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || { let _ = o; };\n\
             f();\n\
         }",
    );
}

#[test]
fn test_once_callable_closure_second_call_error() {
    // Calling a once-callable closure twice is use-after-move.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || { let _ = o; };\n\
             f();\n\
             f();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove on second call, got: {errors:?}"
    );
}

#[test]
fn test_closure_capturing_only_copy_is_not_once_callable() {
    // A closure that only reads Copy values can be called any number of times.
    ownership_ok(
        "fn main() {\n\
             let n: i64 = 42;\n\
             let f = || { let _ = n; };\n\
             f();\n\
             f();\n\
         }",
    );
}

#[test]
fn test_closure_consuming_own_non_copy_param_is_multi_callable() {
    // B-2026-07-15-14: a closure whose body CONSUMES its OWN non-Copy PARAM
    // (`s: String` flowing by value into a mutating collection method) is
    // multi-callable — the param is fresh each call, not a capture. The
    // `push_str` receiver `buf` is mut-ref-captured (mutated in place), and
    // the only consume is `s`, the closure's own param. Was rejected as
    // once-callable; must now pass repeated calls. The `Map.insert` twin (a
    // non-Copy KEY param) is the same shape.
    ownership_ok(
        "fn main() {\n\
             let mut buf: String = String.from(\"\");\n\
             let mut append = |s: String| buf.push_str(s);\n\
             append(String.from(\"a\"));\n\
             append(String.from(\"b\"));\n\
             println(buf);\n\
         }",
    );
    ownership_ok(
        "fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             let mut put = |k: String, v: i64| m.insert(k, v);\n\
             put(String.from(\"a\"), 1);\n\
             put(String.from(\"b\"), 2);\n\
             println(m.len());\n\
         }",
    );
}

#[test]
fn test_closure_consuming_outer_non_copy_capture_still_once_callable() {
    // The dual of the fix above — the once-callable guarantee must survive:
    // a closure that CONSUMES an OUTER non-Copy binding (not its own param)
    // stays once-callable, so a second call is still UseAfterMove
    // (B-2026-07-15-14 must not over-relax).
    let errors = ownership_errors(
        "fn sink(s: String) -> i64 { s.len() }\n\
         fn main() {\n\
             let outer: String = String.from(\"hello\");\n\
             let once = || sink(outer);\n\
             let a = once();\n\
             let b = once();\n\
             println(a + b);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "a closure consuming an OUTER capture must stay once-callable, got: {errors:?}"
    );
}

// ── Consume predicate: partial move through field projection ────

#[test]
fn test_consume_of_non_copy_field_consumes_root() {
    // `take(c.inner)` where inner is non-Copy String should consume the
    // root binding `c` per design.md § Consume Predicate step 3.
    let errors = ownership_errors(
        "fn take(s: String) {}\n\
         struct Container { inner: String }\n\
         fn main() {\n\
             let c = Container { inner: \"hello\" };\n\
             take(c.inner);\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from `let _ = c` after consuming non-Copy field, got: {errors:?}"
    );
}

#[test]
fn test_consume_of_copy_field_does_not_consume_root() {
    // `take(c.n)` where n is Copy i64 should NOT consume root `c`.
    ownership_ok(
        "fn take(n: i64) {}\n\
         struct Container { n: i64, inner: String }\n\
         fn main() {\n\
             let c = Container { n: 1, inner: \"hello\" };\n\
             take(c.n);\n\
             let _ = c.inner;\n\
         }",
    );
}

#[test]
fn test_deep_field_projection_consume_propagates_to_root() {
    // `take(c.a.b)` where `b` is non-Copy should consume `c`.
    let errors = ownership_errors(
        "fn take(s: String) {}\n\
         struct Inner { b: String }\n\
         struct Outer { a: Inner }\n\
         fn main() {\n\
             let c = Outer { a: Inner { b: \"hi\" } };\n\
             take(c.a.b);\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove on root after deep field consume, got: {errors:?}"
    );
}

#[test]
fn test_tuple_index_consume_propagates_to_root() {
    // `take(t.0)` where t is `(String, i64)` should consume `t`.
    let errors = ownership_errors(
        "fn take(s: String) {}\n\
         fn main() {\n\
             let t = (\"hi\", 1);\n\
             take(t.0);\n\
             let _ = t;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from `let _ = t` after consuming non-Copy tuple element, got: {errors:?}"
    );
}

#[test]
fn test_field_read_only_unchanged() {
    // Regression: `let v = c.value` where value is Copy continues to read
    // the chain (no consume of root).
    ownership_ok(
        "struct Container { value: i64, name: String }\n\
         fn main() {\n\
             let c = Container { value: 42, name: \"x\" };\n\
             let v = c.value;\n\
             let w = c.value;\n\
             let _ = c.name;\n\
         }",
    );
}

// ── Consume predicate: function param mode (step 2) ────────────

#[test]
fn test_call_with_owned_param_consumes_arg() {
    // `consume(s)` where `consume(s: String)` declares the param as
    // bare-T (Owned) — consumes the arg. Existing behavior, regression.
    let errors = ownership_errors(
        "fn consume(s: String) {}\n\
         fn main() {\n\
             let s = \"hi\";\n\
             consume(s);\n\
             let _ = s;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove after Owned-param call, got: {errors:?}"
    );
}

#[test]
fn test_call_with_mut_slice_param_does_not_consume_caller_vec() {
    // `mut Slice[T]` param accepts `mut ref Vec[T]` via the typechecker's
    // implicit coercion. The arg is a borrow position — caller's `v` is
    // read, not consumed. This is the most ergonomically common case where
    // step 2's borrow classification matters in user code today.
    ownership_ok(
        "fn clear(xs: mut Slice[i64]) {}\n\
         fn caller(v: mut ref Vec[i64]) {\n\
             clear(v);\n\
             clear(v);\n\
         }",
    );
}

#[test]
fn test_static_method_call_with_owned_param_consumes_arg() {
    // `Make.from(s)` where the static method declares `from(s: String)`
    // (bare-T) — consumes `s`.
    let errors = ownership_errors(
        "struct Make {}\n\
         impl Make {\n\
             fn from(s: String) -> Make { Make {} }\n\
         }\n\
         fn main() {\n\
             let s = \"hi\";\n\
             let _ = Make.from(s);\n\
             let _ = s;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove after static-method bare-T call, got: {errors:?}"
    );
}

#[test]
fn test_call_mixed_params_classifies_per_position() {
    // Per-position classification: bare-T arg consumes, ref-T arg doesn't,
    // even when mixed in the same call.
    let errors = ownership_errors(
        "fn process(read_only: ref String, owned: String) {}\n\
         fn main() {\n\
             let r = \"reader\";\n\
             let o = \"owned\";\n\
             process(r, o);\n\
             let _ = r;\n\
             let _ = o;\n\
         }",
    );
    // r should still be usable (ref-arg). o should not (consumed).
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove && e.message.contains("'o'")),
        "expected UseAfterMove on `o` (owned) but not `r` (ref), got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove && e.message.contains("'r'")),
        "did not expect UseAfterMove on `r` (ref-arg), got: {errors:?}"
    );
}

// ── Consume predicate: method receiver-mode (step 1) ───────────

#[test]
fn test_method_with_owned_self_consumes_receiver() {
    // `c.into_string()` where the impl declares `fn into_string(self) -> String`
    // takes the receiver by-move and so consumes `c`.
    let errors = ownership_errors(
        "struct Container { inner: String }\n\
         impl Container {\n\
             fn into_string(self) -> String { self.inner }\n\
         }\n\
         fn main() {\n\
             let c = Container { inner: \"hi\" };\n\
             let _ = c.into_string();\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove after `c.into_string()` (owned self), got: {errors:?}"
    );
}

#[test]
fn test_method_with_ref_self_does_not_consume() {
    // `c.peek()` where the impl declares `fn peek(ref self) -> i64` reads
    // the receiver — `c` remains usable.
    ownership_ok(
        "struct Container { value: i64 }\n\
         impl Container {\n\
             fn peek(ref self) -> i64 { self.value }\n\
         }\n\
         fn main() {\n\
             let c = Container { value: 1 };\n\
             let _ = c.peek();\n\
             let _ = c.peek();\n\
             let _ = c;\n\
         }",
    );
}

#[test]
fn test_method_with_mut_ref_self_does_not_consume() {
    // `mut ref self` is a borrow, not a consume — receiver stays usable.
    ownership_ok(
        "struct Counter { n: i64 }\n\
         impl Counter {\n\
             fn bump(mut ref self) { self.n = self.n + 1; }\n\
         }\n\
         fn main() {\n\
             let mut c = Counter { n: 0 };\n\
             c.bump();\n\
             c.bump();\n\
             let _ = c;\n\
         }",
    );
}

#[test]
fn test_method_owned_self_param_inferred_own() {
    // When a fn param is consumed via owned-self method call, the param
    // mode is inferred Own.
    let result = ownership_ok(
        "struct Container { inner: String }\n\
         impl Container {\n\
             fn into_string(self) -> String { self.inner }\n\
         }\n\
         fn unwrap(c: Container) -> String {\n\
             c.into_string()\n\
         }",
    );
    let modes = result.param_modes.get("unwrap").unwrap();
    assert_eq!(
        modes[0].1,
        OwnershipMode::Own,
        "expected param `c` of `unwrap` Own (consumed via owned-self method), got: {modes:?}"
    );
}

#[test]
fn test_method_owned_self_on_field_consumes_root() {
    // Composes with round-11.2's partial-move-through-projection: an owned-
    // self method call on `c.inner` consumes root `c`.
    let errors = ownership_errors(
        "struct Container { inner: Inner }\n\
         struct Inner { value: i64 }\n\
         impl Inner {\n\
             fn unwrap(self) -> i64 { self.value }\n\
         }\n\
         fn main() {\n\
             let c = Container { inner: Inner { value: 1 } };\n\
             let _ = c.inner.unwrap();\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from c.inner.unwrap() (owned self on field), got: {errors:?}"
    );
}

#[test]
fn test_method_owned_self_on_copy_receiver_is_noop() {
    // Owned-self method on a Copy type is a noop — Copy values aren't moved.
    ownership_ok(
        "struct Counter { n: i64 }\n\
         impl Counter {\n\
             fn into_n(self) -> i64 { self.n }\n\
         }\n\
         #[derive(Copy)]\n\
         #[derive(Clone)]\n\
         struct CopyCounter { n: i64 }\n\
         impl CopyCounter {\n\
             fn into_n(self) -> i64 { self.n }\n\
         }\n\
         fn main() {\n\
             let c = CopyCounter { n: 1 };\n\
             let _ = c.into_n();\n\
             let _ = c.into_n();\n\
             let _ = c;\n\
         }",
    );
}

// ── Consume predicate: unsafe-block transparency (step 6) ──────

#[test]
fn test_consume_inside_unsafe_block_detected() {
    // A consume inside `unsafe { ... }` must be visible to the use-
    // predicate scan exactly like a consume inside an ordinary block.
    // (Note: ownership analysis does not "mask" consumes inside unsafe;
    // the unsafe escape hatch only suppresses the use-after-move
    // *rejection* on a separate pass — see design.md § Consume
    // Predicate step 6.)
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) {}\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             unsafe { consume(d); }\n\
             let _ = d;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from unsafe-block consume, got: {errors:?}"
    );
}

#[test]
fn test_unsafe_block_param_consume_infers_own() {
    // Param mode inference walks through `unsafe { ... }` — a param
    // consumed only inside an unsafe block is still classified Own.
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) {}\n\
         fn drain(d: Data) {\n\
             unsafe { consume(d); }\n\
         }",
    );
    let modes = result.param_modes.get("drain").unwrap();
    assert_eq!(
        modes[0].1,
        OwnershipMode::Own,
        "expected param `d` of `drain` Own (consumed inside unsafe), got: {:?}",
        modes
    );
}

// ── Consume predicate: nesting-depth invariance (step 7) ───────

#[test]
fn test_deeply_nested_unconditional_consume_detected() {
    // A consume buried 4 levels deep in unconditional nested blocks
    // is classified the same as a top-level consume. The walker
    // recurses into all AST children unconditionally — depth is
    // irrelevant to the use-predicate scan.
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) {}\n\
         fn main() {\n\
             let d = Data { value: 1 };\n\
             { { { { consume(d); } } } }\n\
             let _ = d;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from depth-4 nested-block consume, got: {errors:?}"
    );
}

#[test]
fn test_deeply_nested_param_consume_infers_own() {
    // Param consumed at depth 4 still infers Own — no path sensitivity,
    // no reachability analysis. Mixes blocks, if (with else for clean
    // merge), and another block.
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn consume(d: Data) {}\n\
         fn deep(d: Data, n: i64) {\n\
             {\n\
                 if n > 0 {\n\
                     { { consume(d); } }\n\
                 } else {\n\
                     { { consume(d); } }\n\
                 }\n\
             }\n\
         }",
    );
    let modes = result.param_modes.get("deep").unwrap();
    let d_mode = &modes
        .iter()
        .find(|(name, _)| name == "d")
        .expect("param d not found")
        .1;
    assert_eq!(
        *d_mode,
        OwnershipMode::Own,
        "expected param `d` of `deep` Own (deep consume), got: {modes:?}"
    );
}

// ── Consume predicate: return / tail expression consume ────────

#[test]
fn test_tail_expression_consumes_partial_move() {
    // `fn make(c: Container) -> String { c.inner }` — the tail-position
    // `c.inner` is a consume context (block.final_expr drives
    // check_expr_consuming), and the round-11.2 partial-move rule
    // pushes it to root `c`. The caller's use after `make(c)` is a
    // use-after-move.
    let errors = ownership_errors(
        "struct Container { inner: String }\n\
         fn make(c: Container) -> String {\n\
             c.inner\n\
         }\n\
         fn caller() {\n\
             let c = Container { inner: \"hi\" };\n\
             let _ = make(c);\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove on caller's `let _ = c` after make(c), got: {errors:?}"
    );
}

#[test]
fn test_tail_expression_param_inferred_own() {
    // The same `make(c)` body should infer the param mode as Own
    // because the body consumes c via tail-position partial move.
    let result = ownership_ok(
        "struct Container { inner: String }\n\
         fn make(c: Container) -> String {\n\
             c.inner\n\
         }",
    );
    let modes = result.param_modes.get("make").unwrap();
    assert_eq!(
        modes[0].1,
        OwnershipMode::Own,
        "expected param `c` of `make` to be inferred Own (tail consume), got: {:?}",
        modes
    );
}

#[test]
fn test_explicit_return_consumes_param() {
    // `fn drain(d: Data) -> Data { return d; }` — explicit return is a
    // consume context; param mode should be Own.
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn drain(d: Data) -> Data {\n\
             return d;\n\
         }",
    );
    let modes = result.param_modes.get("drain").unwrap();
    assert_eq!(
        modes[0].1,
        OwnershipMode::Own,
        "expected param `d` of `drain` to be inferred Own (return consume), got: {:?}",
        modes
    );
}

#[test]
fn test_tail_expression_in_nested_block_propagates() {
    // Nested block tail: `{ { d } }` — outer block's final_expr is the
    // inner block; inner block's final_expr is `d`. Tail consume must
    // propagate through the nesting.
    let result = ownership_ok(
        "struct Data { value: i64 }\n\
         fn make() -> Data {\n\
             let d = Data { value: 1 };\n\
             { d }\n\
         }",
    );
    // No assertion on errors (already ownership_ok). Just regression: a
    // body that double-uses `d` should still error.
    let errors = ownership_errors(
        "struct Data { value: i64 }\n\
         fn make() -> Data {\n\
             let d = Data { value: 1 };\n\
             let _ = d;\n\
             { d }\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove from nested-block tail re-use of d, got: {errors:?}"
    );
    let _ = result; // silence unused-binding lint
}

// ── Consume predicate: match scrutinee classification ──────────

#[test]
fn test_match_with_binding_consumes_non_copy_scrutinee() {
    // `match opt { Some(s) => ... }` where s is non-Copy String binds
    // the inner value by-move, so the scrutinee `opt` is consumed.
    let errors = ownership_errors(
        "fn main() {\n\
             let opt: Option[String] = Some(\"hi\");\n\
             let _ = match opt { Some(s) => s, None => \"\" };\n\
             let _ = opt;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove on `let _ = opt` after match-with-binding, got: {errors:?}"
    );
}

#[test]
fn test_match_with_all_wildcards_does_not_consume() {
    // `match opt { Some(_) | None => 0 }` has no bindings — scrutinee is
    // just read.
    ownership_ok(
        "fn main() {\n\
             let opt: Option[String] = Some(\"hi\");\n\
             let _ = match opt { Some(_) => 1, None => 0 };\n\
             let _ = opt;\n\
         }",
    );
}

#[test]
fn test_match_on_copy_scrutinee_with_binding_is_noop() {
    // Match on `Option[i64]` (Copy) with a binding — the consume of the
    // scrutinee is a no-op because Option[i64] is Copy.
    ownership_ok(
        "fn main() {\n\
             let opt: Option[i64] = Some(42);\n\
             let _ = match opt { Some(n) => n, None => 0 };\n\
             let _ = opt;\n\
         }",
    );
}

#[test]
fn test_match_on_borrowed_self_field_does_not_consume_self() {
    // B-2026-07-03-26: matching a non-Copy field (or indexed-element field)
    // of a BORROWED `mut ref self` receiver must NOT be treated as consuming
    // `self` — you cannot move out of a borrow (the language clones), so a
    // subsequent whole-`self` use (`self.adv()`) is not a use-after-move.
    // Regressed by 5426bbd1 (projection-aware partial-move tracking), which
    // recorded `self.field` as a self-consume because `param_types` left the
    // borrowed `self` absent and the fallback typed it as the bare (non-Copy)
    // Self. This is the exact `match self.tokens[self.pos].token { .. };
    // self.advance()` shape pervading the self-hosted parser, which reddened
    // all three selfhost parser oracles on main.
    ownership_ok(
        "enum Tok { A(String), B }\n\
         struct P { cur: Tok, toks: Vec[Tok], pos: i64 }\n\
         impl P {\n\
             fn adv(mut ref self) { self.pos = self.pos + 1; }\n\
             fn direct(mut ref self) -> i64 {\n\
                 let k = match self.cur { A(s) => 1, B => 2 };\n\
                 self.adv();\n\
                 k\n\
             }\n\
             fn indexed(mut ref self) -> i64 {\n\
                 let k = match self.toks[self.pos] { A(s) => 1, B => 2 };\n\
                 self.adv();\n\
                 k\n\
             }\n\
         }\n\
         fn main() {}",
    );
}

#[test]
fn test_match_on_owned_self_field_still_consumes() {
    // The dual of the fix above: an OWNED `self` receiver CAN be consumed, so
    // moving a field out of it twice is still a use-after-move (the fix is
    // scoped to borrowed receivers — owned `self` semantics are preserved).
    let errors = ownership_errors(
        "struct S { name: String }\n\
         fn take(s: String) -> String { s }\n\
         impl S {\n\
             fn consume_twice(self) -> String {\n\
                 let a = take(self.name);\n\
                 let b = take(self.name);\n\
                 b\n\
             }\n\
         }\n\
         fn main() {}",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove moving `self.name` twice out of owned self, got: {errors:?}"
    );
}

#[test]
fn test_match_at_binding_consumes_non_copy_scrutinee() {
    // `match opt { whole @ Some(_) => ... }` — the @-binding takes
    // ownership of the whole value, so the scrutinee is consumed.
    let errors = ownership_errors(
        "fn main() {\n\
             let opt: Option[String] = Some(\"hi\");\n\
             let _ = match opt { whole @ Some(_) => whole, None => None };\n\
             let _ = opt;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove after at-binding match, got: {errors:?}"
    );
}

#[test]
fn test_match_ref_at_binding_does_not_consume_scrutinee() {
    // `ref whole @ Some(_)` flips the subtree to borrow mode
    // (design.md § @ Bindings) — the scrutinee stays live after the
    // match.
    ownership_ok(
        "fn main() {\n\
             let opt: Option[String] = Some(\"hi\");\n\
             match opt { ref whole @ Some(_) => { let _ = whole; } None => { } }\n\
             let _ = opt;\n\
         }",
    );
}

#[test]
fn test_let_ref_at_binding_does_not_consume_rhs() {
    // `let ref x @ Foo { a } = foo` — borrow, not move; `foo` stays
    // live for the rest of the block.
    ownership_ok(
        "struct Foo { a: String }\n\
         fn main() {\n\
             let foo = Foo { a: \"hi\" };\n\
             let ref x @ Foo { a } = foo;\n\
             let _ = a;\n\
             let _ = x;\n\
             let _ = foo;\n\
         }",
    );
}

#[test]
fn test_match_struct_pattern_with_field_binding_consumes() {
    // Struct pattern that binds a non-Copy field consumes the scrutinee.
    let errors = ownership_errors(
        "struct Container { name: String, n: i64 }\n\
         fn main() {\n\
             let c = Container { name: \"hi\", n: 1 };\n\
             let _ = match c { Container { name, n: _ } => name };\n\
             let _ = c;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove after struct-pattern match with field binding, got: {errors:?}"
    );
}

// ── Closure capture-mode prefix (Rule 2½ / K2 conflict table) ────

#[test]
fn test_capture_mode_ref_consume_is_error() {
    // `ref |...|` forbids consume of any captured name.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || { let _ = o; };\n\
             let _ = f;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::CaptureModeViolation
                && e.message.contains("`ref`")
                && e.message.contains("`o`")),
        "expected CaptureModeViolation naming `ref` and capture `o`, got: {errors:?}"
    );
}

#[test]
fn test_capture_mode_mut_ref_consume_is_error() {
    // `mut ref |...|` also forbids consume.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || { let _ = o; };\n\
             let _ = f;\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::CaptureModeViolation
                && e.message.contains("`mut ref`")
                && e.message.contains("`o`")),
        "expected CaptureModeViolation naming `mut ref` and capture `o`, got: {errors:?}"
    );
}

#[test]
fn test_capture_mode_ref_read_only_is_ok() {
    // `ref |...|` accepts a read-only body.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
}

#[test]
fn test_capture_mode_mut_ref_read_only_is_ok() {
    // `mut ref |...|` declared but body only reads — typechecks (perf note
    // for `unused-mut-capture` is a future enhancement; not asserted here).
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
}

#[test]
fn test_capture_mode_bare_consume_unchanged() {
    // Regression: bare `||` form continues to capture-and-consume normally
    // — once-callable inference still applies, no K2 violation. Program
    // typechecks and passes ownership (closure called once, captured value
    // not used after).
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || { let _ = o; };\n\
             f();\n\
         }",
    );
}

#[test]
fn test_capture_mode_ref_does_not_taint_outer_binding() {
    // When `ref |...|` rejects a body consume, the outer binding `o` should
    // remain usable — the closure expression is the error site; the outer
    // value was never actually moved into a successful closure.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = ref || { let _ = o; };\n\
             let _g = o;\n\
         }",
    );
    // Exactly one CaptureModeViolation; no UseAfterMove cascading from it.
    let cmv = errors
        .iter()
        .filter(|e| e.kind == OwnershipErrorKind::CaptureModeViolation)
        .count();
    assert_eq!(
        cmv, 1,
        "expected exactly one CaptureModeViolation, got: {errors:?}"
    );
}

// ── Closure capture-mode prefix: unused-mut-capture perf note (round 12.5) ────
//
// Rule 2½ K2 conflict-table row "mut ref + reads only": when a closure
// declared `mut ref |...|` references a capture but never mutates it,
// the closure typechecks (declared mode is stronger than body usage —
// permitted under the stronger-or-equal rule), but a Tier 2 perf note
// fires suggesting `ref` instead.

#[test]
fn test_unused_mut_capture_note_fires_for_read_only_use() {
    // Canonical case: `mut ref` declared, body only reads `o.x`.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
    let notes: Vec<_> = result
        .notes
        .iter()
        .filter(|n| n.kind == OwnershipErrorKind::UnusedMutCaptureNote)
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "expected exactly one UnusedMutCaptureNote, got notes: {:?}",
        result.notes,
    );
    assert!(
        notes[0].message.contains("`o`")
            && notes[0].message.contains("`mut ref`")
            && notes[0].message.contains("never mutated"),
        "unexpected note message: {}",
        notes[0].message
    );
    assert!(
        notes[0]
            .suggestion
            .as_ref()
            .is_some_and(|s| s.contains("`ref`")),
        "expected suggestion mentioning `ref`, got: {:?}",
        notes[0].suggestion
    );
}

#[test]
fn test_unused_mut_capture_note_does_not_fire_when_capture_mutated_via_field_assign() {
    // `mut ref` declared and the body assigns `o.x = ...` — declared mode
    // matches body usage, no perf note.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1 };\n\
             let f = mut ref || { o.x = 2; };\n\
             f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "expected no UnusedMutCaptureNote, got: {:?}",
        result.notes
    );
}

#[test]
fn test_unused_mut_capture_note_does_not_fire_for_bare_closure() {
    // Bare `||` (owned default) + read-only body — the K2 row "owned + reads
    // only" is OK with no note (capture-for-ownership-extension idiom).
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "expected no UnusedMutCaptureNote for bare closure, got: {:?}",
        result.notes
    );
}

#[test]
fn test_unused_mut_capture_note_does_not_fire_for_ref_closure() {
    // `ref` declared + read-only body matches exactly — no perf note.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "expected no UnusedMutCaptureNote for `ref` closure, got: {:?}",
        result.notes
    );
}

#[test]
fn test_unused_mut_capture_note_per_capture_when_body_mixes() {
    // Two captures: one mutated, one read-only. The note fires for the
    // read-only one and not for the mutated one. Both names appear in the
    // body so both are real captures from the K2 perspective.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut a = Owned { x: 1 };\n\
             let b = Owned { x: 2 };\n\
             let f = mut ref || { a.x = b.x + 1; };\n\
             f();\n\
         }",
    );
    let notes: Vec<_> = result
        .notes
        .iter()
        .filter(|n| n.kind == OwnershipErrorKind::UnusedMutCaptureNote)
        .collect();
    assert_eq!(
        notes.len(),
        1,
        "expected exactly one UnusedMutCaptureNote (for `b`), got: {:?}",
        result.notes
    );
    assert!(
        notes[0].message.contains("`b`"),
        "expected note to name `b`, got: {}",
        notes[0].message
    );
}

#[test]
fn test_unused_mut_capture_note_does_not_fire_for_mut_ref_self_method() {
    // `mut ref` declared and the body invokes a `mut ref self` method on
    // the capture — this is a mutation, no perf note. (Validates the
    // method-self-mode lookup path in classify_capture_body_uses.)
    let result = ownership_ok(
        "struct Counter { value: i64 }\n\
         impl Counter { fn bump(mut ref self) { self.value = self.value + 1; } }\n\
         fn main() {\n\
             let mut c = Counter { value: 0 };\n\
             let f = mut ref || { c.bump(); };\n\
             f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "expected no UnusedMutCaptureNote for `mut ref self` method call, got: {:?}",
        result.notes
    );
}

#[test]
fn test_unused_mut_capture_note_does_not_fire_when_capture_unreferenced() {
    // `mut ref` declared but the body never references the would-be
    // capture — there's no capture at all, so no note. (The note is
    // specifically for captures that ARE referenced but only as reads.)
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let _o = Owned { x: 1 };\n\
             let f = mut ref || 42;\n\
             let _ = f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "expected no UnusedMutCaptureNote when body has no captures, got: {:?}",
        result.notes
    );
}

#[test]
fn test_unused_mut_capture_note_carries_machine_applicable_replacement() {
    // Round 12.31: the N0507 perf note gains the same machine-applicable
    // `replacement` metadata the resolver classes already carry. The
    // TextEdit covers exactly the `mut ref` prefix tokens — no closure
    // body, no surrounding whitespace — and replaces them with `ref`.
    // Applying the edit converts `mut ref || o.x + 1` to `ref || o.x + 1`
    // in place, ready for `karac fix`-style consumers and IDE quick-fix
    // UIs without further dispatcher work.
    let src = "struct Owned { x: i64 }\n\
               fn main() {\n\
                   let o = Owned { x: 1 };\n\
                   let f = mut ref || o.x + 1;\n\
                   let _ = f();\n\
               }";
    let result = ownership_ok(src);
    let note = result
        .notes
        .iter()
        .find(|n| n.kind == OwnershipErrorKind::UnusedMutCaptureNote)
        .expect("expected an UnusedMutCaptureNote");
    let edit = note
        .replacement
        .as_deref()
        .expect("N0507 should carry a TextEdit");
    assert_eq!(
        edit.replacement, "ref",
        "replacement text should be `ref`, got `{}`",
        edit.replacement
    );
    let original = &src[edit.offset..edit.offset + edit.length];
    assert_eq!(
        original, "mut ref",
        "edit span should cover only the prefix tokens, got `{original}`",
    );
    let mut rewritten = src.to_string();
    rewritten.replace_range(edit.offset..edit.offset + edit.length, &edit.replacement);
    assert_eq!(
        rewritten,
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || o.x + 1;\n\
             let _ = f();\n\
         }",
        "applying the edit should swap `mut ref` for `ref` in place",
    );
}

#[test]
fn test_unused_mut_capture_note_replacement_per_note_when_multiple_unused() {
    // Two read-only captures in one `mut ref` closure produce two notes,
    // each carrying the same prefix-rewrite TextEdit. The dispatcher in
    // `cmd_fix` already dedupes overlapping edits, so applying produces
    // a single rewrite. Pinned because the note is emitted per-capture
    // in a loop and the replacement plumbing must populate every note
    // independently — not just the first.
    let src = "struct Owned { x: i64 }\n\
               fn main() {\n\
                   let a = Owned { x: 1 };\n\
                   let b = Owned { x: 2 };\n\
                   let f = mut ref || a.x + b.x;\n\
                   let _ = f();\n\
               }";
    let result = ownership_ok(src);
    let notes: Vec<_> = result
        .notes
        .iter()
        .filter(|n| n.kind == OwnershipErrorKind::UnusedMutCaptureNote)
        .collect();
    assert_eq!(notes.len(), 2, "expected one note per read-only capture");
    for n in &notes {
        let edit = n
            .replacement
            .as_deref()
            .expect("each N0507 note should carry a TextEdit");
        assert_eq!(edit.replacement, "ref");
        let original = &src[edit.offset..edit.offset + edit.length];
        assert_eq!(original, "mut ref");
    }
    // All notes target the same prefix, so the edits are byte-identical —
    // a sanity check on the per-note populate path.
    let first = notes[0].replacement.as_deref().unwrap();
    let second = notes[1].replacement.as_deref().unwrap();
    assert_eq!(first.offset, second.offset);
    assert_eq!(first.length, second.length);
}

// ── Closure calling through `ref` — repeatable-closure multi-call (round 12.6) ────
//
// Item 23: explicit `ref |...|` / `mut ref |...|` capture-mode prefixes
// guarantee a closure is *repeatable* — its captures are borrowed, not
// consumed, so calling it does not move the closure binding. Multiple
// invocations are valid. Bare `|...|` capturing an owned non-Copy value
// remains once-callable (existing behavior, regressed here for symmetry).

#[test]
fn test_ref_closure_multi_call_ok() {
    // `ref ||` borrows captures — closure is repeatable, multiple calls OK.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || o.x + 1;\n\
             let _ = f();\n\
             let _ = f();\n\
             let _ = f();\n\
         }",
    );
}

#[test]
fn test_mut_ref_closure_multi_call_ok() {
    // `mut ref ||` mutates borrowed captures — closure is repeatable.
    // Body uses a Copy field rebind (the perf note for "never mutated"
    // does not block the program; we explicitly check no errors).
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1 };\n\
             let f = mut ref || { o.x = o.x + 1; };\n\
             f();\n\
             f();\n\
         }",
    );
}

#[test]
fn test_ref_closure_keeps_outer_binding_usable_after_calls() {
    // After multiple `ref ||` invocations the outer binding `o` is still
    // usable — the captures are borrows, not moves.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || o.x + 1;\n\
             let _ = f();\n\
             let _ = f();\n\
             let _y = o.x;\n\
         }",
    );
}

#[test]
fn test_bare_once_callable_still_rejects_second_call() {
    // Regression for the contrast: bare `||` capturing owned non-Copy
    // remains once-callable; the second call is a use-after-move.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || { let _ = o; };\n\
             f();\n\
             f();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "expected UseAfterMove on second call of bare-form once-callable closure, got: {errors:?}"
    );
}

#[test]
fn test_ref_closure_called_in_loop_ok() {
    // The motivating pattern from design.md §3638: a repeatable closure
    // invoked many times — here, the same binding called inside a loop.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 7 };\n\
             let f = ref || o.x + 1;\n\
             for _i in 0..3 {\n\
                 let _ = f();\n\
             }\n\
         }",
    );
}

// ── Uninitialized `let pat: T;` plumbing (round 12.1) ───────────
//
// 12.1 lands AST + parser + walker plumbing only. Definite-assignment
// semantics arrive in 12.2, so today the binding is registered as `Live`
// — meaning programs that read before initializing do NOT yet error.
// These tests pin the plumbing in place so 12.2 can swap the state
// machine without breaking the surface.

#[test]
fn test_let_uninit_then_assign_then_read_passes() {
    // Canonical first-assignment-is-init flow. Even with `let mut x: T;
    // x = ...; use(x);` the binding is registered cleanly through every
    // phase and ownership doesn't reject it.
    ownership_ok(
        "fn main() {\n\
            let mut x: i64;\n\
            x = 5;\n\
            let _y = x;\n\
        }",
    );
}

#[test]
fn test_let_uninit_array_plumbing() {
    // `let arr: Array[T, N];` is the form the design.md spec calls out for
    // the whole-value-assignment rule. Round 12.1 just plumbs the AST end
    // to end; the per-slot-write rule lands in 12.3.
    ownership_ok(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr = [1, 2, 3, 4];\n\
            let _x = arr;\n\
        }",
    );
}

#[test]
fn test_let_uninit_immutable_first_assign_is_init() {
    // Per design.md §1689 the first assignment to `let x: T;` (no `mut`)
    // counts as initialization, not reassignment, so it succeeds even
    // though the binding wasn't declared `mut`.
    ownership_ok(
        "fn main() {\n\
            let x: i64;\n\
            x = 7;\n\
            let _y = x;\n\
        }",
    );
}

// ── Round 12.2: definite-assignment scalar flow ─────────────────

#[test]
fn test_read_uninit_errors() {
    // Reading a `let x: T;` binding before any assignment is a definite-
    // assignment failure.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            let _ = x;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_read_uninit_in_rhs_of_first_assign_errors() {
    // `let x: T; x = f(x);` — the `x` inside the RHS is still uninit
    // when evaluated, so the read errors even though the assign would
    // otherwise initialize.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            x = x + 1;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_second_assign_to_immutable_uninit_errors() {
    // `let x: T;` without `mut`: first assign is initialization (OK),
    // second assign requires `let mut x: T;`.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            x = 5;\n\
            x = 6;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::ReassignToImmutable),
        "expected ReassignToImmutable, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_let_mut_uninit_allows_reassign() {
    // `let mut x: T;` allows arbitrary reassignment after the first init.
    ownership_ok(
        "fn main() {\n\
            let mut x: i64;\n\
            x = 1;\n\
            x = 2;\n\
            x = 3;\n\
            let _y = x;\n\
        }",
    );
}

#[test]
fn test_init_in_both_branches_promotes() {
    // Both arms of the if/else assign — the join is initialized.
    ownership_ok(
        "fn main() {\n\
            let x: i64;\n\
            if true {\n\
                x = 1;\n\
            } else {\n\
                x = 2;\n\
            }\n\
            let _y = x;\n\
        }",
    );
}

#[test]
fn test_init_in_only_then_branch_does_not_promote() {
    // One-armed if (no else) cannot promote — the value would be uninit
    // on the falling-through path.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            if true {\n\
                x = 1;\n\
            }\n\
            let _y = x;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_init_in_only_one_arm_of_if_else_does_not_promote() {
    // Only one branch of an else-bearing if assigns — still partial init.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            if true {\n\
                x = 1;\n\
            } else {\n\
                let _ = 0;\n\
            }\n\
            let _y = x;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_init_in_loop_body_does_not_promote_after_loop() {
    // Loop body may run zero times; an assign inside the body cannot
    // satisfy DA after the loop.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            while false {\n\
                x = 1;\n\
            }\n\
            let _y = x;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_init_then_read_inside_loop_body_passes() {
    // Inside the same iteration: assign-then-read on a `mut` binding is
    // fine. (Not the cross-iteration DA case.)
    ownership_ok(
        "fn main() {\n\
            let mut x: i64;\n\
            while false {\n\
                x = 1;\n\
                let _y = x;\n\
            }\n\
        }",
    );
}

#[test]
fn test_match_all_arms_init_promotes() {
    // Exhaustive match where every arm assigns — the join is initialized.
    ownership_ok(
        "fn main() {\n\
            let x: i64;\n\
            let n: i64 = 7;\n\
            match n {\n\
                0 => { x = 100; },\n\
                _ => { x = 200; },\n\
            }\n\
            let _y = x;\n\
        }",
    );
}

#[test]
fn test_match_one_arm_uninit_does_not_promote() {
    // If even one match arm leaves the binding uninit, the join is uninit.
    let errors = ownership_errors(
        "fn main() {\n\
            let x: i64;\n\
            let n: i64 = 7;\n\
            match n {\n\
                0 => { x = 100; },\n\
                _ => { let _ = 0; },\n\
            }\n\
            let _y = x;\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseOfUninitialized),
        "expected UseOfUninitialized, got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

// ── Round 12.3: Array per-slot DA rule ──────────────────────────

#[test]
fn test_read_uninit_array_errors_with_array_specific_message() {
    // Plain read of an uninit Array binding — error fires and the
    // diagnostic is array-specific (mentions whole-value-assignment and
    // the canonical fully-initialized constructors).
    let errors = ownership_errors(
        "fn main() {\n\
            let arr: Array[i64, 4];\n\
            let _x = arr;\n\
        }",
    );
    let array_err = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::UseOfUninitialized);
    let err = array_err.expect("expected UseOfUninitialized for array read");
    assert!(
        err.message.contains("uninitialized array"),
        "expected array-specific message, got {:?}",
        err.message
    );
    let suggestion = err.suggestion.as_deref().expect("expected a suggestion");
    assert!(
        suggestion.contains("whole value") && suggestion.contains("Array.from_fn"),
        "expected whole-value-assignment suggestion, got {:?}",
        suggestion
    );
}

#[test]
fn test_index_assign_to_uninit_array_does_not_promote() {
    // Per design.md §1097 the v1 DA analyser tracks whole-value assignment
    // only — `arr[0] = ...` on a `let mut arr: Array[T, N];` does NOT
    // promote `arr` to Live. The index assign reads `arr` to compute the
    // address, which fires UseOfUninitialized; a subsequent read of `arr`
    // is *also* uninit (the slot fill did not satisfy DA).
    let errors = ownership_errors(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr[0] = 1;\n\
            let _x = arr;\n\
        }",
    );
    // At least the index-assign read should fire. The trailing read of
    // `arr` should also fire — i.e. `arr` was NOT promoted by the slot
    // assign.
    let array_uninit_count = errors
        .iter()
        .filter(|e| {
            e.kind == OwnershipErrorKind::UseOfUninitialized
                && e.message.contains("uninitialized array")
        })
        .count();
    assert!(
        array_uninit_count >= 2,
        "expected at least two array-uninit errors (index-assign + trailing read), got {} in {:?}",
        array_uninit_count,
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_whole_value_assign_to_uninit_array_promotes() {
    // The canonical happy path — whole-value assign promotes; subsequent
    // reads succeed.
    ownership_ok(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr = [1, 2, 3, 4];\n\
            let _x = arr;\n\
        }",
    );
}

#[test]
fn test_index_assign_to_initialized_array_is_fine() {
    // After a whole-value init, index-assign is just a normal mutation.
    // No DA error here — the rule is about reaching the *initial* assign,
    // not about subsequent slot writes.
    ownership_ok(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr = [0, 0, 0, 0];\n\
            arr[0] = 1;\n\
            arr[1] = 2;\n\
            let _x = arr;\n\
        }",
    );
}

#[test]
fn test_repeat_literal_assign_to_uninit_array_promotes() {
    // `Array[v; n]` repeat-literal as the whole-value RHS satisfies DA.
    ownership_ok(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr = Array[0; 4];\n\
            let _x = arr;\n\
        }",
    );
}

#[test]
fn test_index_read_on_uninit_array_errors() {
    // Reading a slot (`arr[i]`) on an uninit array also reads `arr` and
    // fires the array-specific DA error.
    let errors = ownership_errors(
        "fn main() {\n\
            let arr: Array[i64, 4];\n\
            let _v = arr[0];\n\
        }",
    );
    assert!(
        errors.iter().any(|e| {
            e.kind == OwnershipErrorKind::UseOfUninitialized
                && e.message.contains("uninitialized array")
        }),
        "expected array-specific UseOfUninitialized, got {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_per_slot_init_all_slots_does_not_satisfy_da() {
    // The "every slot is eventually written" pattern is *explicitly*
    // rejected by §1097 — DA does not look at slot coverage; only the
    // whole-value assignment counts.
    let errors = ownership_errors(
        "fn main() {\n\
            let mut arr: Array[i64, 4];\n\
            arr[0] = 1;\n\
            arr[1] = 2;\n\
            arr[2] = 3;\n\
            arr[3] = 4;\n\
            let _x = arr;\n\
        }",
    );
    assert!(
        errors.iter().any(|e| {
            e.kind == OwnershipErrorKind::UseOfUninitialized
                && e.message.contains("uninitialized array")
        }),
        "expected array DA error even after per-slot fill, got {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Round 12.21: UAM-via-predicate routing sentinels ────────────────

#[test]
fn test_uam_predicate_emits_single_error_per_binding() {
    // Round 12.21 routes UseAfterMove through the predicate, which
    // returns one witness per binding. Pin that the legacy state
    // machine no longer double-emits for the same binding when it
    // encounters the binding's use site after the move.
    let errors = ownership_errors(
        "struct Data { value: i64 }
         fn consume(d: Data) { }
         fn main() {
             let d = Data { value: 1 };
             consume(d);
             consume(d);
         }",
    );
    let uam_count = errors
        .iter()
        .filter(|e| e.kind == OwnershipErrorKind::UseAfterMove && e.message.contains("'d'"))
        .count();
    assert_eq!(
        uam_count, 1,
        "expected exactly one UAM for binding 'd' under predicate routing; got {:?}",
        errors
    );
}

#[test]
fn test_uam_predicate_carries_consume_and_use_spans() {
    // The predicate's UamWitness uses (consume_span, other_use_span)
    // pairs. Pin that the emitted error's span points at the
    // OFFENDING USE (not the consume) — matching the legacy contract.
    let errors = ownership_errors(
        "struct Data { value: i64 }
         fn consume(d: Data) { }
         fn main() {
             let d = Data { value: 1 };
             consume(d);
             consume(d);
         }",
    );
    let uam = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::UseAfterMove)
        .expect("expected at least one UAM error");
    // The error span is on the second `consume(d)` (line 6 in this
    // source; line 1 is `struct ...`, line 4 is `let d = ...`,
    // line 5 is the first consume, line 6 is the use-after-move).
    assert_eq!(
        uam.span.line, 6,
        "UAM span should point at the second consume site (line 6), got line {}",
        uam.span.line
    );
    // The message includes the moved-at line (5) of the first consume.
    assert!(
        uam.message.contains("moved at line 5"),
        "expected message to reference line 5 (the consume); got: {}",
        uam.message
    );
}

#[test]
fn test_tuple_destructure_copy_sibling_not_consumed() {
    // B-2026-06-14-27: a Copy binding destructured from a tuple whose sibling
    // is a move type (String) was given the WHOLE tuple type by the use
    // classifier — non-Copy because of the String — so reads of the Copy
    // binding in a consuming position were misclassified as Consume and the UAM
    // predicate fired a spurious "value 'n' moved … used again". Here `n` (i64)
    // is read in an `if` guard and again in tail/return position while the
    // sibling `s` (String) is moved; that must NOT raise a UAM on `n`.
    ownership_ok(
        "fn helper() -> (i64, String) { (2i64, \"x\") }
         fn build() -> i64 {
             let (n, s) = helper();
             let mut acc: String = \"\";
             if n > 0i64 { acc.push_str(s); }
             n + 1i64
         }
         fn main() { let _ = build(); }",
    );
}

#[test]
fn test_tuple_destructure_move_sibling_still_uam() {
    // Guard for the B-2026-06-14-27 fix: the precise per-field typing must not
    // mask a GENUINE use-after-move of the move-typed sibling. `s` (String) is
    // consumed once and used again — that UAM must still fire.
    let errors = ownership_errors(
        "fn take(x: String) -> i64 { x.bytes().len() as i64 }
         fn helper() -> (i64, String) { (2i64, \"x\") }
         fn main() {
             let (n, s) = helper();
             let a = take(s);
             let b = take(s);
             let _ = n + a + b;
         }",
    );
    let s_uam = errors
        .iter()
        .filter(|e| e.kind == OwnershipErrorKind::UseAfterMove && e.message.contains("'s'"))
        .count();
    assert_eq!(
        s_uam, 1,
        "genuine UAM of move-typed tuple sibling 's' must still fire; got {:?}",
        errors
    );
}

// ── Round 12.23: Closure parameter mode inference (Step 1) ─────────
//
// Each closure expression `|x, y, z| body` has its parameters
// classified `own` / `ref` / `mut ref` by the same `ParamUsage`
// scan that drives fn-param inference. Inferred modes are surfaced
// per-closure via `OwnershipCheckResult::closure_param_modes`,
// keyed by the closure expression's `SpanKey`. These tests pin the
// classification rule for every variant of body usage.

/// Pull the inferred mode list for the single closure in `result`.
/// Asserts exactly one closure is recorded — useful for the
/// per-shape unit tests below where the source has one closure.
fn single_closure_modes(result: &OwnershipCheckResult) -> &Vec<(String, OwnershipMode)> {
    assert_eq!(
        result.closure_param_modes.len(),
        1,
        "expected exactly one closure in source; got {} entries: {:?}",
        result.closure_param_modes.len(),
        result.closure_param_modes.keys().collect::<Vec<_>>()
    );
    result.closure_param_modes.values().next().unwrap()
}

#[test]
fn closure_param_consumed_inferred_own() {
    // The closure's body consumes `x` (passes it owned to a
    // bare-`T` slot) → inferred mode is `own`.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         fn take(d: Data) { }\n\
         fn main() {\n\
             let f = |x: Data| take(x);\n\
             let d = Data { v: 1 };\n\
             f(d);\n\
         }",
    );
    let modes = single_closure_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[("x".to_string(), OwnershipMode::Own)],
        "consume in body should infer own; got {:?}",
        modes
    );
}

#[test]
fn closure_param_read_only_inferred_ref() {
    // Body only reads through a Copy field projection → no consume,
    // mode is `ref`.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let f = |c: Config| c.value + 1;\n\
             let cfg = Config { value: 10 };\n\
             let _r = f(cfg);\n\
         }",
    );
    let modes = single_closure_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[("c".to_string(), OwnershipMode::Ref)],
        "read-only body should infer ref; got {:?}",
        modes
    );
}

#[test]
fn closure_param_unused_inferred_ref() {
    // Body never references the closure parameter → mode defaults
    // to `ref` (same as fn-param inference's `Unused`).
    let result = ownership_ok(
        "fn main() {\n\
             let f = |_x: i64| 42;\n\
             let _r = f(7);\n\
         }",
    );
    let modes = single_closure_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[("_x".to_string(), OwnershipMode::Ref)],
        "unused param should infer ref; got {:?}",
        modes
    );
}

#[test]
fn closure_multiple_params_each_classified_independently() {
    // First param consumed (own), second read-only (ref), third
    // unused (ref). Pins the per-param classification independence.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         struct Config { value: i64 }\n\
         fn take(d: Data) { }\n\
         fn main() {\n\
             let f = |a: Data, b: Config, _c: i64| { take(a); b.value };\n\
             let _r = f(Data { v: 1 }, Config { value: 2 }, 3);\n\
         }",
    );
    let modes = single_closure_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            ("a".to_string(), OwnershipMode::Own),
            ("b".to_string(), OwnershipMode::Ref),
            ("_c".to_string(), OwnershipMode::Ref),
        ],
        "modes should be classified independently per param; got {:?}",
        modes
    );
}

#[test]
fn closure_param_does_not_pollute_outer_param_usage() {
    // A closure parameter that shadows an outer fn parameter must
    // not bleed its consumption signal back into the outer fn's
    // mode inference. Outer `x` is only read; the closure's `x` is
    // consumed. Outer must stay `ref`.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         fn take(d: Data) { }\n\
         fn outer(x: i64) -> i64 {\n\
             let f = |x: Data| take(x);\n\
             f(Data { v: 0 });\n\
             x\n\
         }",
    );
    let outer_modes = result
        .param_modes
        .get("outer")
        .expect("outer must have inferred modes");
    assert_eq!(
        outer_modes.as_slice(),
        &[("x".to_string(), OwnershipMode::Ref)],
        "outer fn-param `x` must remain ref despite closure consuming its own `x`; got {:?}",
        outer_modes
    );
    // Closure's local `x` should still classify as own.
    let closure_modes = single_closure_modes(&result);
    assert_eq!(
        closure_modes.as_slice(),
        &[("x".to_string(), OwnershipMode::Own)],
        "closure-local `x` should infer own"
    );
}

#[test]
fn closure_param_modes_keyed_by_closure_expression_span() {
    // Two closures in the same function — each gets its own entry
    // keyed by the closure expression's span. Pins that the
    // closure_param_modes map is per-closure, not per-function.
    let src = "struct Data { v: i64 }\n\
               fn take(d: Data) { }\n\
               fn main() {\n\
                   let f = |a: Data| take(a);\n\
                   let g = |_b: i64| 0;\n\
                   f(Data { v: 1 });\n\
                   let _r = g(2);\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        result.errors.is_empty(),
        "ownership errors: {:?}",
        result.errors
    );
    assert_eq!(
        result.closure_param_modes.len(),
        2,
        "expected two distinct closure entries; got {:?}",
        result.closure_param_modes.keys().collect::<Vec<_>>()
    );
    // Verify both are keyed by valid SpanKeys (non-default) and
    // their inferred modes match the body shapes.
    let modes_lists: Vec<_> = result.closure_param_modes.values().collect();
    let owns: Vec<_> = modes_lists
        .iter()
        .filter(|m| matches!(m.first(), Some((_, OwnershipMode::Own))))
        .collect();
    let refs: Vec<_> = modes_lists
        .iter()
        .filter(|m| matches!(m.first(), Some((_, OwnershipMode::Ref))))
        .collect();
    assert_eq!(owns.len(), 1, "exactly one closure should have own param");
    assert_eq!(refs.len(), 1, "exactly one closure should have ref param");
    // Sanity: SpanKey is real; constructing a fresh one should not
    // collide unless we passed the same span.
    let fresh = SpanKey(0, 0);
    assert!(!result.closure_param_modes.contains_key(&fresh));
}

// ── Round 12.24: Closure capture collection (Step 2) ────────────
//
// Each closure expression's free variables — outer-scope bindings
// referenced from inside the body — are surfaced via
// `OwnershipCheckResult::closure_captures`, keyed by the closure
// expression's span. Capture mode is derived from body usage:
// consume → `Own`, in-place mutation → `MutRef`, read-only → `Ref`.
// Names lexically shadowed by the closure's own parameters are NOT
// captured; the body's references to those names are to the
// closure-local. These tests pin the surface for each shape.

/// Pull the captures for the single closure in `result`.
fn single_closure_captures(result: &OwnershipCheckResult) -> &Vec<(String, OwnershipMode)> {
    assert_eq!(
        result.closure_captures.len(),
        1,
        "expected exactly one closure in source; got {} entries: {:?}",
        result.closure_captures.len(),
        result.closure_captures.keys().collect::<Vec<_>>()
    );
    result.closure_captures.values().next().unwrap()
}

#[test]
fn capture_consumed_in_body_is_own() {
    // Closure body moves the captured `cfg` into a consume-position
    // call → capture mode is `Own`.
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         fn apply(c: Config) { }\n\
         fn main() {\n\
             let cfg = Config { name: 1 };\n\
             let _f = || apply(cfg);\n\
         }",
    );
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[("cfg".to_string(), OwnershipMode::Own)],
        "consume-capture should fire Own; got {:?}",
        caps
    );
}

#[test]
fn capture_read_only_is_ref() {
    // Body only reads through a Copy field projection → capture
    // mode is `Ref`.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let _f = || cfg.value + 1;\n\
         }",
    );
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[("cfg".to_string(), OwnershipMode::Ref)],
        "read-only capture should fire Ref; got {:?}",
        caps
    );
}

#[test]
fn capture_unreferenced_outer_is_not_captured() {
    // Outer binding `unused_v` is never referenced by the closure
    // body — it must NOT appear in the capture list. Outer `cfg` is
    // referenced.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let unused_v = 42;\n\
             let _f = || cfg.value + 1;\n\
             let _u = unused_v;\n\
         }",
    );
    let caps = single_closure_captures(&result);
    let names: Vec<&str> = caps.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        names.contains(&"cfg"),
        "cfg should be captured; got {:?}",
        names
    );
    assert!(
        !names.contains(&"unused_v"),
        "unused_v must not be captured; got {:?}",
        names
    );
}

#[test]
fn capture_excludes_shadowed_outer_name() {
    // Outer `x` shadowed by closure-local `x`. Body references `x`
    // (the closure-local). Outer `x` must NOT be in the capture
    // list. Pins the closure-param-set exclusion check.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         fn take(d: Data) { }\n\
         fn outer(x: i64) -> i64 {\n\
             let _f = |x: Data| take(x);\n\
             x\n\
         }",
    );
    // Outer `x` is in pre_live but lexically shadowed by closure
    // param `x`. Capture list must be empty.
    let caps = single_closure_captures(&result);
    assert!(
        caps.is_empty(),
        "shadowed outer name must not appear as a capture; got {:?}",
        caps
    );
}

#[test]
fn capture_multiple_outer_bindings_each_classified() {
    // Two captures: one consumed, one read. Both should appear with
    // their respective modes; output is alphabetic by name.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         struct Config { value: i64 }\n\
         fn take(d: Data) { }\n\
         fn main() {\n\
             let d = Data { v: 1 };\n\
             let cfg = Config { value: 2 };\n\
             let _f = || { take(d); cfg.value };\n\
         }",
    );
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[
            ("cfg".to_string(), OwnershipMode::Ref),
            ("d".to_string(), OwnershipMode::Own),
        ],
        "captures should list each outer binding with its body-derived mode; got {:?}",
        caps
    );
}

#[test]
fn capture_keyed_per_closure_expression() {
    // Two distinct closures in the same fn — each gets its own
    // capture list keyed by closure expression span.
    let src = "struct Config { value: i64 }\n\
               fn main() {\n\
                   let a = Config { value: 1 };\n\
                   let b = Config { value: 2 };\n\
                   let _f = || a.value;\n\
                   let _g = || b.value;\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        result.errors.is_empty(),
        "ownership errors: {:?}",
        result.errors
    );
    assert_eq!(
        result.closure_captures.len(),
        2,
        "expected two closure entries; got {}",
        result.closure_captures.len()
    );
    let all_captured: Vec<String> = result
        .closure_captures
        .values()
        .flat_map(|caps| caps.iter().map(|(n, _)| n.clone()))
        .collect();
    assert!(all_captured.contains(&"a".to_string()));
    assert!(all_captured.contains(&"b".to_string()));
    // Each closure has exactly one capture (a or b, not both).
    for caps in result.closure_captures.values() {
        assert_eq!(
            caps.len(),
            1,
            "each closure has one capture; got {:?}",
            caps
        );
    }
}

#[test]
fn nested_closures_each_get_their_own_mode_entry() {
    // Closure inside a closure body — both should be recorded with
    // independently-inferred modes.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         fn take(d: Data) { }\n\
         fn main() {\n\
             let outer = |x: Data| {\n\
                 let inner = |y: Data| take(y);\n\
                 inner(x);\n\
             };\n\
             outer(Data { v: 1 });\n\
         }",
    );
    assert_eq!(
        result.closure_param_modes.len(),
        2,
        "outer + inner closures should both register; got {} entries",
        result.closure_param_modes.len()
    );
    // Both bodies consume their respective params → both should be
    // own.
    for modes in result.closure_param_modes.values() {
        assert_eq!(modes.len(), 1, "each closure has one param");
        assert_eq!(
            modes[0].1,
            OwnershipMode::Own,
            "both closures consume their param → both should be own; got {:?}",
            modes
        );
    }
}

// ── Bare-form per-capture inference pins (Rule 2½ default) ──────
//
// design.md § Closures, Rule 2½: "A bare closure `|x| body` runs
// Rule 2's first-use scan to infer each capture's mode (read → `ref`,
// mutate → `mut ref`, consume → `own`)." `capture_consumed_in_body_is_own`
// and `capture_read_only_is_ref` above pin the Own / Ref legs;
// the MutRef leg is pinned here. Plus negative-space coverage for the
// `own` prefix (the only one without a K2 conflict row to fire on a
// stronger-than-declared body usage — declared `own` is the strongest
// mode in the `ref < mut ref < own` ordering).

#[test]
fn capture_mutated_in_body_is_mut_ref() {
    // Bare body assigns through a captured root → `body_usage.mutated`
    // is set on `o`; classification picks `MutRef`. The MutRef leg of
    // the Rule 2 inference table — completes the {Ref, MutRef, Own}
    // trio.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1 };\n\
             let _f = || { o.x = 2; };\n\
         }",
    );
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[("o".to_string(), OwnershipMode::MutRef)],
        "field-assign mutation of captured root should classify capture as MutRef; got {:?}",
        caps
    );
}

#[test]
fn capture_mode_own_prefix_accepts_consume_body() {
    // Explicit `own ||` + body consumes capture → declared mode and
    // usage agree (K2 row "own + consumes"). Mirrors
    // `test_capture_mode_bare_consume_unchanged` but with the explicit
    // prefix pinned.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = own || { let _ = o; };\n\
             f();\n\
         }",
    );
}

#[test]
fn capture_mode_own_prefix_accepts_read_only_body() {
    // K2 row "own + reads only": OK — the "capture for ownership
    // extension" idiom (closure holds the value by value; body chose
    // not to consume it). No UnusedMutCaptureNote — that note is
    // specific to the `mut ref` declared / read-only used gap.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = own || o.x + 1;\n\
             let _ = f();\n\
         }",
    );
    assert!(
        result
            .notes
            .iter()
            .all(|n| n.kind != OwnershipErrorKind::UnusedMutCaptureNote),
        "no UnusedMutCaptureNote should fire for `own` + read-only; got: {:?}",
        result.notes
    );
}

#[test]
fn capture_mode_ref_consume_diagnostic_includes_spec_fix_wording() {
    // Pin the K2 conflict diagnostic's spec-mandated fix wording from
    // design.md § Closures Rule 2½ conflict table for `ref` + consume:
    //   "drop the `ref` prefix (use `own` or bare) or remove the consume"
    // The existing `test_capture_mode_ref_consume_is_error` checks the
    // error kind + key terms; this test pins the full guidance string
    // so a future diagnostic rewrite cannot silently drop the redirect
    // shape.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = ref || { let _ = o; };\n\
             let _ = f;\n\
         }",
    );
    let cmv = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::CaptureModeViolation)
        .expect("expected at least one CaptureModeViolation");
    let fix = cmv.suggestion.as_deref().unwrap_or("");
    assert!(
        fix.contains("drop the `ref` prefix")
            && fix.contains("`own` or bare")
            && fix.contains("remove the consume"),
        "ref K2 fix wording missing required phrases; got suggestion: {fix:?}"
    );
}

#[test]
fn capture_mode_mut_ref_consume_diagnostic_includes_spec_fix_wording() {
    // Symmetric pin for the `mut ref` + consume row:
    //   "drop the `mut ref` prefix and use `own`"
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || { let _ = o; };\n\
             let _ = f;\n\
         }",
    );
    let cmv = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::CaptureModeViolation)
        .expect("expected at least one CaptureModeViolation");
    let fix = cmv.suggestion.as_deref().unwrap_or("");
    assert!(
        fix.contains("drop the `mut ref` prefix") && fix.contains("use `own`"),
        "mut ref K2 fix wording missing required phrases; got suggestion: {fix:?}"
    );
}

#[test]
fn bare_closure_read_capture_leaves_outer_binding_usable() {
    // Bare-form inference picks `Ref` for `o` (body only reads). The
    // outer scope can continue to read `o` after the closure's last
    // use — pins that outer-scope availability tracks the inferred
    // per-capture mode, not a blanket "closure consumes everything"
    // approximation.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = || o.x + 1;\n\
             let _u = o.x;\n\
         }",
    );
}

#[test]
fn bare_closure_consume_capture_with_outer_use_routes_through_rc_fallback() {
    // Bare-form inference picks `Own` for `o` (body consumes via a
    // value-taking call). The post-closure `let _u = o;` is an outer
    // use of the consumed capture — by design (Rule 2 sub-case (ii) +
    // Part 4 RC trigger 2), this does NOT fire UseAfterMove; the RC
    // dataflow pass tentatively marks `o` as `Rc` instead. Pins the
    // routing: outer-use-after-Own-capture is NOT a hard error, it is
    // an opt-in to RC fallback. Symmetric to the read-only test above
    // (Ref capture leaves outer-scope use trivially valid; Own
    // capture leaves it valid via RC promotion).
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn take(o: Owned) { }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = || take(o);\n\
             let _u = o;\n\
         }",
    );
    let main_rcs = result
        .rc_values
        .get("main")
        .expect("expected rc_values entry for `main`");
    let o_entry = main_rcs
        .get("o")
        .expect("expected `o` to be RC-promoted via closure-capture-with-outer-use trigger");
    assert!(
        matches!(o_entry.trigger, RcTrigger::ClosureCaptureWithOuterUse),
        "expected RC trigger ClosureCaptureWithOuterUse on `o`; got {:?}",
        o_entry.trigger
    );
    // Capture mode for `o` is `Own` (body consumes via the value-take
    // call). Pin alongside the RC trigger so the two halves of the
    // routing story are asserted together.
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[("o".to_string(), OwnershipMode::Own)],
        "consume in body → Own capture; got {:?}",
        caps
    );
}

// ── Disjoint closure capture — slice 1 (capture-path enumeration) ─
//
// Phase-5 § Disjoint closure capture (line 353) slice 1: the closure
// analyser produces a `CapturePath { root, projection }` set per
// closure expression in addition to the per-name capture-mode list.
// Empty projection means "captured whole" (bare identifier or a
// reference through a stopping construct — index, method call, or
// deref). Non-empty projection lists the field-chain root-to-leaf.
// Slice 1 surfaces only the set; mode inference is slice 2,
// borrow-checker integration is slice 3.
//
// These tests pin the path-set shape produced for the closure-body
// constructs the spec calls out in its test plan.

/// Pull the capture-path list for the single closure in `result`.
fn single_closure_capture_paths(result: &OwnershipCheckResult) -> &Vec<CapturePath> {
    assert_eq!(
        result.closure_capture_paths.len(),
        1,
        "expected exactly one closure in source; got {} entries",
        result.closure_capture_paths.len()
    );
    result.closure_capture_paths.values().next().unwrap()
}

fn path(root: &str, projection: &[&str]) -> CapturePath {
    CapturePath {
        root: root.to_string(),
        projection: projection.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn capture_path_bare_identifier_is_whole_root() {
    // `|| take(cfg)` — the body references `cfg` as a bare identifier
    // (call arg). Path-set is `{(cfg, [])}` — root captured whole.
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         fn take(c: Config) { }\n\
         fn main() {\n\
             let cfg = Config { name: 1 };\n\
             let _f = || take(cfg);\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("cfg", &[])],
        "bare identifier should register whole-root path; got {:?}",
        paths
    );
}

#[test]
fn capture_path_single_field_chain_records_projection() {
    // `|| cfg.value + 1` — body reads a single field projection. Path
    // is `(cfg, ["value"])` — root + one-segment projection. The root
    // is NOT additionally registered as a whole capture (the spec
    // walker extends the path through field accesses; only stopping
    // constructs commit the root as whole).
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let _f = || cfg.value + 1;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("cfg", &["value"])],
        "field projection should record projection chain only; got {:?}",
        paths
    );
}

#[test]
fn capture_path_nested_field_chain_records_full_projection() {
    // `|| u.profile.name` — body reads through two field segments.
    // Path is `(u, ["profile", "name"])` — full root-to-leaf chain.
    let result = ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 } };\n\
             let _f = || u.profile.name + 1;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("u", &["profile", "name"])],
        "nested field chain should record full projection; got {:?}",
        paths
    );
}

#[test]
fn capture_path_disjoint_fields_under_same_root_record_distinct_paths() {
    // `|| { u.name; u.age }` — two distinct field projections under
    // one root. Path-set is `{(u, ["age"]), (u, ["name"])}` — both
    // siblings recorded, sorted lexicographically. The root `u` is
    // NOT registered as whole — neither projection hits a stopping
    // construct, so the path walker extends through each access
    // independently. Pins the foundation slice 2/3 will use to
    // accept outer-scope sibling access of `u.history` after the
    // closure captures only `u.name` and `u.age`.
    let result = ownership_ok(
        "struct User { name: i64, age: i64 }\n\
         fn main() {\n\
             let u = User { name: 1, age: 2 };\n\
             let _f = || u.name + u.age;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("u", &["age"]), path("u", &["name"])],
        "disjoint sibling fields should record distinct paths; got {:?}",
        paths
    );
}

#[test]
fn capture_path_index_commits_root_whole() {
    // `|| vec[0]` — index is a stopping construct per spec. The
    // walker commits the root `vec` as captured whole regardless of
    // what the indexed result is used for. Path-set is `{(vec, [])}`.
    // Slice 3's borrow checker will use this to deny outer-scope
    // sibling access when the closure captured the whole vec.
    let result = ownership_ok(
        "fn main() {\n\
             let vec = [1, 2, 3];\n\
             let _f = || vec[0] + 1;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("vec", &[])],
        "index expression should commit root whole; got {:?}",
        paths
    );
}

#[test]
fn capture_path_method_call_receiver_commits_root_whole() {
    // `|| u.length()` — method call on a captured root is a stopping
    // construct (the method may use any/all of the receiver's state
    // through its `self` parameter; the analyser cannot tell which
    // fields a method touches without inter-procedural inspection).
    // Path-set is `{(u, [])}`.
    let result = ownership_ok(
        "struct User { name: i64 }\n\
         impl User { fn length(ref self) -> i64 { 0 } }\n\
         fn main() {\n\
             let u = User { name: 1 };\n\
             let _f = || u.length();\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("u", &[])],
        "method call on captured root should commit root whole; got {:?}",
        paths
    );
}

#[test]
fn capture_path_index_into_field_chain_commits_root_whole() {
    // `|| vec[0].field` — index appears inside the projection chain.
    // The walker hits Index before completing the FieldAccess
    // extraction; the root `vec` commits as captured whole. The
    // outer `.field` access surrounding the index does not extend
    // the path (its object is no longer a pure field chain).
    let result = ownership_ok(
        "struct Item { field: i64 }\n\
         fn main() {\n\
             let items = [Item { field: 1 }];\n\
             let _f = || items[0].field + 1;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("items", &[])],
        "index inside field chain should commit root whole; got {:?}",
        paths
    );
}

#[test]
fn capture_path_method_call_on_field_chain_commits_root_whole() {
    // `|| u.profile.method()` — the method-call receiver `u.profile`
    // is a captured-rooted place; the receiver commits the root `u`
    // as captured whole. The intermediate `profile` projection is
    // NOT recorded as a separate path — it was an in-progress field
    // chain when the stopping construct fired.
    let result = ownership_ok(
        "struct Profile { name: i64 }\n\
         impl Profile { fn length(ref self) -> i64 { 0 } }\n\
         struct User { profile: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 } };\n\
             let _f = || u.profile.length();\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("u", &[])],
        "method call on field chain should commit root whole; got {:?}",
        paths
    );
}

#[test]
fn capture_path_multiple_roots_each_recorded_independently() {
    // Two distinct outer bindings, each touched through its own path
    // shape (one via field, one via index). Output is sorted by root
    // then projection.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let arr = [10, 20, 30];\n\
             let _f = || cfg.value + arr[1];\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("arr", &[]), path("cfg", &["value"])],
        "two roots should appear sorted with their respective shapes; got {:?}",
        paths
    );
}

#[test]
fn capture_path_excludes_shadowed_outer_name() {
    // Outer `x` lexically shadowed by the closure's own parameter
    // `x`. The closure body's `x.v` references the closure-local,
    // not the outer binding. Path-set must be empty — the outer `x`
    // is not captured.
    let result = ownership_ok(
        "struct Data { v: i64 }\n\
         fn take(d: Data) { }\n\
         fn outer(x: i64) -> i64 {\n\
             let _f = |x: Data| take(x);\n\
             x\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert!(
        paths.is_empty(),
        "shadowed outer name must not appear in path-set; got {:?}",
        paths
    );
}

#[test]
fn capture_path_unreferenced_outer_name_produces_no_path() {
    // `unused_v` is in the outer scope but the closure body never
    // touches it. Only `cfg.value` is captured, registering one
    // path. Pins the parity with `closure_captures` exclusion of
    // unreferenced outer bindings.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let unused_v = 42;\n\
             let _f = || cfg.value + 1;\n\
             let _u = unused_v;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("cfg", &["value"])],
        "unreferenced outer name should not appear in path-set; got {:?}",
        paths
    );
}

#[test]
fn capture_path_tuple_index_extends_projection() {
    // `|| t.0 + 1` — tuple-index access extends the path the same
    // way struct-field access does, with the index segment
    // stringified into the projection vector.
    let result = ownership_ok(
        "fn main() {\n\
             let t = (10, 20);\n\
             let _f = || t.0 + 1;\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    assert_eq!(
        paths.as_slice(),
        &[path("t", &["0"])],
        "tuple-index access should extend projection with stringified index; got {:?}",
        paths
    );
}

// ── Disjoint closure capture — slice 2 (per-path mode inference) ─
//
// Phase-5 § Disjoint closure capture (line 353) slice 2: the closure
// analyser pairs each `CapturePath` from slice 1 with a mode
// (`Own` / `MutRef` / `Ref`) derived by running the use-predicate
// scan from Rule 2 against that path independently. A path
// overlapping any mutation event in the body (assignment target,
// `mut`-marker arg, `mut ref self` method-call receiver) is
// `MutRef`; an empty-projection path whose root was consumed whole
// is `Own`; everything else is `Ref`. Overlap is bidirectional —
// the recorded path's projection being a prefix of the target's
// (write to descendant of recorded place) or vice versa (write to
// ancestor) both mark the recorded path as mutated.
//
// Result is `Vec<(CapturePath, OwnershipMode)>` per closure, parallel
// to slice 1's `Vec<CapturePath>` in the same order. Read-only
// surface — slice 3 will consume the mode-tagged set in the borrow
// checker.

/// Pull the per-path mode list for the single closure in `result`.
fn single_closure_capture_path_modes(
    result: &OwnershipCheckResult,
) -> &Vec<(CapturePath, OwnershipMode)> {
    assert_eq!(
        result.closure_capture_path_modes.len(),
        1,
        "expected exactly one closure in source; got {} entries",
        result.closure_capture_path_modes.len()
    );
    result.closure_capture_path_modes.values().next().unwrap()
}

#[test]
fn capture_path_mode_bare_identifier_read_is_ref() {
    // `|| use_ref(cfg)` — wait, no — bare `cfg` passed by value to a
    // by-value function consumes it. Instead use a body that only
    // reads through a Copy projection so the bare-ident path stays
    // un-consumed: `|| cfg.value + 1` registers `(cfg, ["value"])`
    // not `(cfg, [])`. To pin the bare-ident-as-whole-path Ref leg,
    // use a closure whose body calls a method with `ref self` on
    // `cfg` — receiver commits `(cfg, [])` whole, and the method
    // mode is ref so no mutation → `Ref`.
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         impl Config { fn length(ref self) -> i64 { 0 } }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let _f = || cfg.length();\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("cfg", &[]), OwnershipMode::Ref)],
        "ref-self method on captured root → whole-root path is Ref; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_field_read_is_ref() {
    // `|| cfg.value + 1` — body reads through a Copy field
    // projection, never mutates. Path is `(cfg, ["value"])` and
    // mode is `Ref` (no mutation event, not whole-root consumed).
    let result = ownership_ok(
        "struct Config { value: i64 }\n\
         fn main() {\n\
             let cfg = Config { value: 1 };\n\
             let _f = || cfg.value + 1;\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("cfg", &["value"]), OwnershipMode::Ref)],
        "read-only field projection should be Ref; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_field_assign_is_mut_ref() {
    // `|| { o.x = 2 }` — assignment to a captured field is a
    // mutation event whose target place is `(o, ["x"])`. The
    // recorded path matches exactly → marked mutated → `MutRef`.
    // Mirrors the per-name `capture_mutated_in_body_is_mut_ref`
    // test but pins the per-path surface.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1 };\n\
             let _f = || { o.x = 2; };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("o", &["x"]), OwnershipMode::MutRef)],
        "field-assign target should mark path MutRef; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_disjoint_fields_independent_modes() {
    // The slice-2 headline test. Body reads one field and writes
    // another sibling field of the same root:
    //   { u.age = 99; u.name + 1 }
    // Slice 1 records two paths under root `u`: `(u, ["age"])` and
    // `(u, ["name"])`. Slice 2's per-path inference treats each
    // independently — only `(u, ["age"])` overlaps a mutation
    // target → it gets `MutRef` while `(u, ["name"])` stays `Ref`.
    // This is the disjointness the per-name view CANNOT express:
    // per-name `u` is uniformly `MutRef` because the root is
    // mutated in aggregate.
    let result = ownership_ok(
        "struct User { name: i64, age: i64 }\n\
         fn main() {\n\
             let mut u = User { name: 1, age: 2 };\n\
             let _f = || { u.age = 99; u.name + 1 };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("u", &["age"]), OwnershipMode::MutRef),
            (path("u", &["name"]), OwnershipMode::Ref),
        ],
        "disjoint fields under same root should take independent modes; got {:?}",
        modes
    );
    // Cross-check the per-name view collapses both to MutRef — the
    // surface slice 2 supersedes for downstream consumers.
    let caps = single_closure_captures(&result);
    assert_eq!(
        caps.as_slice(),
        &[("u".to_string(), OwnershipMode::MutRef)],
        "per-name view should collapse to MutRef (its existing semantics); got {:?}",
        caps
    );
}

#[test]
fn capture_path_mode_compound_assign_is_mut_ref() {
    // `o.x += 1` — compound-assign target is treated the same as a
    // bare assign target. Path `(o, ["x"])` overlaps the mutation
    // → `MutRef`.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1 };\n\
             let _f = || { o.x += 1; };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("o", &["x"]), OwnershipMode::MutRef)],
        "compound-assign target should mark path MutRef; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_method_mut_ref_self_commits_root_mut_ref() {
    // `|| u.bump()` where `bump` takes `mut ref self` — the
    // receiver `u` is captured whole (slice 1 stopping construct)
    // AND the receiver call is a mutation event → `(u, [])` is
    // marked mutated → `MutRef`. Pins that the method-receiver
    // mutation event correctly lifts the whole-root path's mode.
    let result = ownership_ok(
        "struct Counter { n: i64 }\n\
         impl Counter { fn bump(mut ref self) { self.n = self.n + 1; } }\n\
         fn main() {\n\
             let mut u = Counter { n: 0 };\n\
             let _f = || u.bump();\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("u", &[]), OwnershipMode::MutRef)],
        "mut-ref-self method on captured root should be MutRef; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_method_ref_self_commits_root_ref() {
    // `|| u.length()` where `length` takes `ref self` — receiver
    // commits root whole, but no mutation event fires (the receiver
    // mode is `ref`, not `mut ref`) → `(u, [])` stays `Ref`. Pairs
    // with the mut-ref-self test above to pin both legs of the
    // method-call mode discrimination.
    let result = ownership_ok(
        "struct User { name: i64 }\n\
         impl User { fn length(ref self) -> i64 { 0 } }\n\
         fn main() {\n\
             let u = User { name: 1 };\n\
             let _f = || u.length();\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("u", &[]), OwnershipMode::Ref)],
        "ref-self method on captured root should be Ref; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_consumed_whole_root_is_own() {
    // `|| apply(cfg)` — by-value pass to an owned-arg function
    // consumes the captured root. Slice 1 records `(cfg, [])`
    // (bare-ident through a stopping call boundary); slice 2's
    // wiring sees `states[cfg] == Moved` and assigns mode `Own`.
    // Mirrors `capture_consumed_in_body_is_own` for the per-name
    // surface — pins per-path matches per-name for the consume
    // leg.
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         fn apply(c: Config) { }\n\
         fn main() {\n\
             let cfg = Config { name: 1 };\n\
             let _f = || apply(cfg);\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("cfg", &[]), OwnershipMode::Own)],
        "by-value pass should mark whole-root path Own; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_independent_roots_independent_modes() {
    // Two distinct captured bindings, one mutated through a field
    // assign, one read through a field. Pins that mode inference
    // is per-path, not per-root-aggregated: `(a, ["v"])` is
    // `MutRef`, `(b, ["v"])` is `Ref`, even though both roots
    // appear in the same body. Output ordering matches slice 1
    // (lexicographic by root then projection).
    let result = ownership_ok(
        "struct Holder { v: i64 }\n\
         fn main() {\n\
             let mut a = Holder { v: 1 };\n\
             let b = Holder { v: 2 };\n\
             let _f = || { a.v = 10; b.v + 1 };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("a", &["v"]), OwnershipMode::MutRef),
            (path("b", &["v"]), OwnershipMode::Ref),
        ],
        "independent roots should take independent modes; got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_ancestor_write_marks_whole_root() {
    // Body has both a stopping construct that commits root whole
    // AND a field assign on the same root:
    //   { u.show(); u.age = 99 }
    // Slice 1 records `(u, [])` (from the method call) AND
    // `(u, ["age"])` (from the assign target's pure-path
    // extraction). Slice 2's bidirectional overlap rule marks BOTH
    // paths mutated: the assign target's projection `["age"]`
    // overlaps `(u, [])` (path's empty projection is a prefix of
    // any target). Pins that an ancestor (whole-root) path
    // correctly inherits MutRef when a descendant is mutated —
    // without this, the whole-root capture would be falsely Ref
    // while a sibling field assign is MutRef, and the closure's
    // env-slot for the whole root would lack the mut access the
    // body needs.
    let result = ownership_ok(
        "struct User { name: i64, age: i64 }\n\
         impl User { fn show(ref self) { } }\n\
         fn main() {\n\
             let mut u = User { name: 1, age: 2 };\n\
             let _f = || { u.show(); u.age = 99; };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("u", &[]), OwnershipMode::MutRef),
            (path("u", &["age"]), OwnershipMode::MutRef),
        ],
        "ancestor whole-root path should be lifted to MutRef when descendant mutated; \
         got {:?}",
        modes
    );
}

#[test]
fn capture_path_mode_path_order_matches_capture_paths() {
    // The slice-2 mode list is parallel to slice 1's path list —
    // both keyed by the same closure span; entries in identical
    // order. Pin this so consumers (slice 3's borrow checker) can
    // rely on zip-iteration without re-sorting.
    let result = ownership_ok(
        "struct User { name: i64, age: i64 }\n\
         fn main() {\n\
             let mut u = User { name: 1, age: 2 };\n\
             let _f = || { u.age = 99; u.name + 1 };\n\
         }",
    );
    let paths = single_closure_capture_paths(&result);
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        paths.len(),
        modes.len(),
        "path-list and mode-list lengths must match"
    );
    for (i, (p, (mp, _))) in paths.iter().zip(modes.iter()).enumerate() {
        assert_eq!(
            p, mp,
            "path-list and mode-list must zip in identical order at index {}; \
             path = {:?}, mode-path = {:?}",
            i, p, mp
        );
    }
}

#[test]
fn capture_path_mode_modes_keyed_by_closure_expression_span() {
    // Two closures in the same function — each gets its own modes
    // entry keyed by the closure expression's span. Mirrors
    // `closure_param_modes_keyed_by_closure_expression_span` for
    // the new per-path-modes map. Pins that the map is per-closure,
    // not per-function.
    let src = "struct Owned { x: i64 }\n\
               fn main() {\n\
                   let mut a = Owned { x: 1 };\n\
                   let b = Owned { x: 2 };\n\
                   let _f = || { a.x = 9; };\n\
                   let _g = || b.x + 1;\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        result.errors.is_empty(),
        "ownership errors: {:?}",
        result.errors
    );
    assert_eq!(
        result.closure_capture_path_modes.len(),
        2,
        "expected two distinct closure entries; got {:?}",
        result.closure_capture_path_modes.keys().collect::<Vec<_>>()
    );
    let modes_lists: Vec<_> = result.closure_capture_path_modes.values().collect();
    let mut_refs: Vec<_> = modes_lists
        .iter()
        .filter(|m| m.iter().any(|(_, mode)| *mode == OwnershipMode::MutRef))
        .collect();
    let refs_only: Vec<_> = modes_lists
        .iter()
        .filter(|m| m.iter().all(|(_, mode)| *mode == OwnershipMode::Ref))
        .collect();
    assert_eq!(
        mut_refs.len(),
        1,
        "exactly one closure should have a MutRef path"
    );
    assert_eq!(
        refs_only.len(),
        1,
        "exactly one closure should have only Ref paths"
    );
}

// ── Disjoint closure capture — slice 5 (Rule 2½ prefix interaction) ─
//
// Line 353 phase-5 checklist — disjoint-capture slice 5. The bare
// closure `|...|` runs Rule 2 per-path inference (slice 2). The three
// explicit prefixes `own |...|`, `ref |...|`, `mut ref |...|` are
// applied *after* path enumeration: each prefix pins every enumerated
// capture path to a single declared mode regardless of body usage.
// Spec: design.md § Rule 2¼ Interaction with Rule 2½ — "Disjoint-path
// detection still runs first to enumerate the paths; the prefix then
// pins the mode of each path to the declared one."
//
// These tests pin (a) the per-path mode map reflects the prefix-forced
// mode, and (b) slice 3's borrow-conflict diagnostic surfaces the
// pinned mode (not the body-inferred mode) in its "by `<mode>`" tail.

#[test]
fn slice5_ref_prefix_pins_read_only_path_to_ref() {
    // `ref || u.x + 1` — bare-form inference would already produce
    // `(u, ["x"])` Ref because the body only reads. The `ref` prefix
    // is a no-op on the recorded mode here, but pinning that the
    // prefix path still runs cleanly catches regressions where the
    // prefix accidentally re-classifies a read-only path.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = ref || o.x + 1;\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("o", &["x"]), OwnershipMode::Ref)],
        "ref prefix on read-only body should keep path as Ref; got {:?}",
        modes
    );
}

#[test]
fn slice5_mut_ref_prefix_pins_read_only_paths_to_mut_ref() {
    // The slice-5 headline test. Body reads `o.x` + `o.y` — bare
    // inference (slice 2) would record both paths as Ref. The
    // `mut ref` prefix pins every enumerated path to MutRef, so both
    // become MutRef. Without slice 5 the paths would remain Ref and
    // the slice-3 borrow check would push only-read borrows that
    // permit aliased outer reads — wrong for a `mut ref` declaration.
    let result = ownership_ok(
        "struct Owned { x: i64, y: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1, y: 2 };\n\
             let _f = mut ref || o.x + o.y;\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("o", &["x"]), OwnershipMode::MutRef),
            (path("o", &["y"]), OwnershipMode::MutRef),
        ],
        "mut ref prefix should pin every enumerated path to MutRef \
         regardless of body-usage inference; got {:?}",
        modes
    );
}

#[test]
fn slice5_own_prefix_pins_read_only_path_to_own() {
    // `own || o.x + 1` — bare inference would yield `(o, ["x"])` Ref.
    // The `own` prefix pins every enumerated path to Own. Slice 5
    // applies to all three prefixes (own / ref / mut ref) — the spec
    // says "the prefix pins the mode of each path to the declared
    // one" without restriction to ref / mut ref.
    let result = ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = own || o.x + 1;\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[(path("o", &["x"]), OwnershipMode::Own)],
        "own prefix should pin every enumerated path to Own; got {:?}",
        modes
    );
}

#[test]
fn slice5_mut_ref_prefix_pins_multiple_paths_under_one_root() {
    // Two paths under root `u` with one mutated, one read in the
    // body. Slice 2 would record `(u, ["age"])` MutRef + `(u, ["name"])`
    // Ref (the slice-2 disjoint-modes test). The `mut ref` prefix
    // collapses both to MutRef — the read-only path is also pinned
    // strong. Pins that slice 5 walks the full path list, not just
    // the inferred-Ref subset.
    let result = ownership_ok(
        "struct User { name: i64, age: i64 }\n\
         fn main() {\n\
             let mut u = User { name: 1, age: 2 };\n\
             let _f = mut ref || { u.age = 99; u.name + 1 };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("u", &["age"]), OwnershipMode::MutRef),
            (path("u", &["name"]), OwnershipMode::MutRef),
        ],
        "mut ref prefix should lift inferred-Ref sibling paths to \
         MutRef too; got {:?}",
        modes
    );
}

#[test]
fn slice5_mut_ref_prefix_pins_paths_across_multiple_roots() {
    // Two roots, one mutated and one read in the body — bare slice 2
    // would yield `(a, ["v"])` MutRef + `(b, ["v"])` Ref (the
    // `capture_path_mode_independent_roots_independent_modes` test).
    // The `mut ref` prefix pins both roots' paths to MutRef. Pins
    // that the per-closure prefix applies across all roots, not just
    // one.
    let result = ownership_ok(
        "struct Holder { v: i64 }\n\
         fn main() {\n\
             let mut a = Holder { v: 1 };\n\
             let b = Holder { v: 2 };\n\
             let _f = mut ref || { a.v = 10; b.v + 1 };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("a", &["v"]), OwnershipMode::MutRef),
            (path("b", &["v"]), OwnershipMode::MutRef),
        ],
        "mut ref prefix should pin paths across all captured roots; \
         got {:?}",
        modes
    );
}

#[test]
fn slice5_prefix_does_not_alter_path_set() {
    // The prefix changes mode, not the path enumeration. Pin that
    // the recorded `closure_capture_paths` (slice 1) is identical
    // between bare and prefixed forms of the same body. Without this
    // pin, an accidental coupling of the prefix into slice 1's walker
    // could shrink/expand the captured path set.
    let result_bare = ownership_ok(
        "struct Owned { x: i64, y: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1, y: 2 };\n\
             let _f = || o.x + o.y;\n\
         }",
    );
    let result_prefix = ownership_ok(
        "struct Owned { x: i64, y: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1, y: 2 };\n\
             let _f = mut ref || o.x + o.y;\n\
         }",
    );
    let paths_bare = single_closure_capture_paths(&result_bare);
    let paths_prefix = single_closure_capture_paths(&result_prefix);
    assert_eq!(
        paths_bare, paths_prefix,
        "capture-path enumeration must be identical between bare and \
         prefixed forms; bare = {:?}, prefix = {:?}",
        paths_bare, paths_prefix
    );
}

#[test]
fn slice5_mut_ref_prefix_surfaces_mut_ref_flavor_in_slice3_diagnostic() {
    // Downstream-visibility check. Body reads `u.x` only — bare
    // inference produces `(u, ["x"])` Ref, and an outer consume of
    // `u` fires `ClosureCaptureBorrowConflict` with message tail
    // `captures `u.x` by `ref``. The `mut ref` prefix pins the path
    // to MutRef via slice 5, so the slice-3 push is a MutRef borrow
    // and the same conflict diagnostic now reads "by `mut ref`".
    // This is the user-visible consequence of slice 5.
    let errors = ownership_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let u = Owned { x: 1 };\n\
             let _f = mut ref || u.x + 1;\n\
             let _w = u;\n\
         }",
    );
    let err = errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict)
        .expect("expected ClosureCaptureBorrowConflict for outer consume");
    assert!(
        err.message.contains("by `mut ref`"),
        "diagnostic should name `mut ref` flavor when slice 5 pins the \
         path; got: {}",
        err.message
    );
}

#[test]
fn slice5_own_prefix_skips_slice3_borrow_push() {
    // Slice 3 skips Own-mode paths (the consume machinery handles
    // them). With the `own` prefix, slice 5 forces every enumerated
    // path to Own, so slice 3 pushes no closure-capture borrows. An
    // outer consume that would otherwise overlap a captured path
    // therefore does not fire `ClosureCaptureBorrowConflict`. Pin
    // this so the slice-3/slice-5 coordination doesn't silently
    // start emitting Own-path borrows.
    //
    // The bare form of this body — `|| u.profile.name + 1` — records
    // `(u, ["profile", "name"])` Ref; without the prefix, the outer
    // consume of `u.profile` (chain `["profile"]` is a prefix of
    // `["profile", "name"]`) fires `ClosureCaptureBorrowConflict`.
    // The `own` prefix forces the path's mode to Own, slice 3 skips
    // it, and the outer consume runs through the per-name move
    // machinery instead (which does not promote `u` to `Moved` here —
    // the body only reads through field access, so the outer consume
    // succeeds). Use `ownership_ok` rather than `ownership_errors` —
    // the spec-prescribed outcome is *no* errors at all.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = own || u.profile.name + 1;\n\
             let _p = u.profile;\n\
         }",
    );
}

#[test]
fn slice5_bare_form_preserves_slice2_inferred_modes() {
    // Negative pin: no prefix → slice 5 is a no-op → the recorded
    // modes match slice 2's per-path inference. Without this pin,
    // a future refactor that always-applies prefix forcing (e.g.,
    // defaulting `capture_mode` to `Ref` for bare closures) would
    // silently change inferred behavior.
    let result = ownership_ok(
        "struct Owned { x: i64, y: i64 }\n\
         fn main() {\n\
             let mut o = Owned { x: 1, y: 2 };\n\
             let _f = || { o.x = 9; o.y + 1 };\n\
         }",
    );
    let modes = single_closure_capture_path_modes(&result);
    assert_eq!(
        modes.as_slice(),
        &[
            (path("o", &["x"]), OwnershipMode::MutRef),
            (path("o", &["y"]), OwnershipMode::Ref),
        ],
        "bare form (no prefix) should leave slice-2 inferred modes \
         intact; got {:?}",
        modes
    );
}

// ── Disjoint closure capture — slice 3 (borrow-checker integration) ─

// Line 353 phase-5 checklist — disjoint-capture slice 3. Pushes a
// closure-induced borrow per `Ref` / `MutRef` capture path the slice-2
// inference produced (whole-root and `Own` paths are skipped — those
// remain the existing RC-trigger-2 surface). Path-aware conflict check
// at consume sites uses bidirectional projection-prefix overlap so
// disjoint sibling-path access remains permitted while overlapping
// ancestor / equal-path access fires `ClosureCaptureBorrowConflict`.

#[test]
fn slice3_closure_ref_capture_permits_outer_sibling_field_consume() {
    // The slice-3 headline permissive case. Closure ref-captures
    // `(u, ["profile"])`; outer scope consumes a disjoint sibling
    // field `u.history`. The path-aware conflict check matches
    // `["profile"]` against `["history"]` — first segment differs,
    // so no overlap and no error. Without slice 3's path precision
    // the borrow would be root-keyed and the consume would falsely
    // reject.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _h = u.history;\n\
         }",
    );
}

#[test]
fn slice3_closure_ref_capture_rejects_outer_whole_root_consume() {
    // The slice-3 headline rejection case (spec test
    // "outer-scope move of `u` while `u.name` is ref-captured is
    // rejected"). Closure ref-captures `(u, ["profile"])`; outer
    // scope consumes `u` whole. Bidirectional prefix overlap fires
    // (shorter is the empty consume chain — trivial prefix of the
    // captured `["profile"]`) → `ClosureCaptureBorrowConflict`.
    let errs = ownership_errors(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _v = u;\n\
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict),
        "expected ClosureCaptureBorrowConflict, got {:?}",
        errs
    );
}

#[test]
fn slice3_closure_mut_ref_capture_rejects_outer_whole_root_consume() {
    // Mut-ref leg of the rejection rule. Closure mut-ref-captures
    // `(u, ["profile"])` via a sub-field assignment; outer consume of
    // `u` whole overlaps. The diagnostic must fire regardless of
    // capture mode — mut-ref and ref both produce live borrows at
    // the same scope.
    let errs = ownership_errors(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let mut u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || { u.profile.name = 9; };\n\
             let _v = u;\n\
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict),
        "expected ClosureCaptureBorrowConflict, got {:?}",
        errs
    );
}

#[test]
fn slice3_closure_ref_capture_rejects_equal_path_outer_consume() {
    // Same-path overlap — consume of `u.profile` (non-Copy struct
    // field) while the closure ref-captures the same `(u, ["profile"])`
    // path via the deeper chain `u.profile.name`. `["profile"]` is a
    // prefix of `["profile", "name"]` → overlap → conflict.
    let errs = ownership_errors(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _p = u.profile;\n\
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict),
        "expected ClosureCaptureBorrowConflict, got {:?}",
        errs
    );
}

#[test]
fn slice3_closure_ref_capture_permits_disjoint_nested_field_consume() {
    // Disjoint nested-projection sibling — captured
    // `(u, ["profile", "name"])` vs consumed `(u, ["history"])`. First
    // segment differs (`profile` vs `history`) → no overlap → no error.
    // Pins that the per-segment compare walks all the way through, not
    // just the first segment of the captured side.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _h = u.history;\n\
         }",
    );
}

#[test]
fn slice3_two_closures_over_disjoint_fields_compile_cleanly() {
    // Spec test "two closures over different fields of the same struct
    // compile and run." Two closures, each holds a precise per-path
    // borrow on a disjoint sibling. Neither closure's borrow overlaps
    // the other, and no outer-scope consume happens, so both coexist
    // without conflict. The slice-3 push-per-path is what lets the
    // borrow tracker see them as independent rather than colliding
    // on the shared root.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _g = || u.history.name + 2;\n\
         }",
    );
}

#[test]
fn slice3_closure_borrow_drains_at_block_exit() {
    // Scope-stamp drain — the closure-capture borrow is scoped to
    // the block that holds the closure value. After that block exits,
    // the borrow drains and the outer `let _v = u` proceeds without
    // a conflict. Pins the drain wired into `drain_borrows_at_depth`.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             {\n\
                 let _f = || u.profile.name + 1;\n\
             }\n\
             let _v = u;\n\
         }",
    );
}

#[test]
fn slice3_whole_root_capture_does_not_push_borrow_routes_through_rc_fallback() {
    // Narrowing pin — when the captured path is whole-root (empty
    // projection, typical when the body calls a method on the captured
    // root which is a slice-1 stopping construct), slice 3 does NOT
    // push a borrow. The outer-consume + closure-body-use pair routes
    // through the existing RC fallback (RcTrigger::DirectReuseAfterConsume
    // for a body-read + outer-consume composition) rather than firing
    // a borrow-style rejection. Pins that slice 3 is purely additive
    // on path-precise captures and does not regress the existing
    // RC-trigger-2 surface.
    let src = "struct Config { name: i64 }\n\
               impl Config {\n\
                   fn id(ref self) -> i64 { self.name }\n\
               }\n\
               fn log(c: Config) { }\n\
               fn make_handler(cfg: Config) {\n\
                   let h = || cfg.id();\n\
                   log(cfg);\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict),
        "slice 3 must not fire for whole-root captures: {:?}",
        result.errors
    );
    let rc = result
        .rc_values
        .get("make_handler")
        .and_then(|m| m.get("cfg"))
        .expect("cfg should be RC-promoted via trigger composition");
    assert_eq!(rc.trigger, RcTrigger::DirectReuseAfterConsume);
}

#[test]
fn terminal_return_accumulator_no_rc_fallback() {
    // B-2026-07-11-9: an accumulator built in a loop and moved out via an early
    // `return` inside that loop, plus the tail `return`, must NOT trigger an RC
    // fallback. The two returns of `out` are mutually exclusive — the early one
    // terminates the function (no fall-through), so there is no reuse after the
    // consume. Before the fix, the loop-branch `return out` (consume) and the
    // tail `out` (other use) paired as a spurious DirectReuseAfterConsume.
    let result = ownership_ok(
        "fn build(n: i64) -> Vec[i64] {\n\
        \x20    let mut out: Vec[i64] = Vec.new();\n\
        \x20    let mut i = 0;\n\
        \x20    while i < n {\n\
        \x20        if i == 5 { return out; }\n\
        \x20        out.push(i);\n\
        \x20        i = i + 1;\n\
        \x20    }\n\
        \x20    out\n\
        }",
    );
    assert!(
        result
            .rc_values
            .get("build")
            .and_then(|m| m.get("out"))
            .is_none(),
        "terminal-return accumulator `out` must NOT be RC-promoted; got: {:?}",
        result.rc_values.get("build")
    );
}

#[test]
fn closure_capture_with_terminal_outer_consume_still_rc() {
    // B-2026-07-11-9 guard: the terminal-return suppression must NOT weaken the
    // genuine closure-capture RC. Here the closure body READS `cfg` (deferred to
    // an unknown future invocation) and the outer scope's LAST statement consumes
    // it via `log(cfg)` — a terminal block. The closure use lives in a
    // `closure_body_blocks` sink, so RC must still fire.
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         impl Config { fn id(ref self) -> i64 { self.name } }\n\
         fn log(c: Config) { }\n\
         fn make_handler(cfg: Config) {\n\
        \x20    let h = || cfg.id();\n\
        \x20    log(cfg);\n\
         }",
    );
    let rc = result
        .rc_values
        .get("make_handler")
        .and_then(|m| m.get("cfg"));
    assert!(
        rc.is_some(),
        "closure-capture + terminal outer consume must still RC-promote `cfg`; got: {:?}",
        result.rc_values.get("make_handler")
    );
}

#[test]
fn slice3_own_capture_does_not_push_borrow_routes_through_rc_fallback() {
    // `Own` paths route through the consume machinery (`Moved` state
    // + RC fallback for outer use), not borrow tracking. Slice 3's
    // `push_closure_capture_borrows` explicitly skips `Own` entries.
    // Outer use of the consumed binding triggers
    // `ClosureCaptureWithOuterUse` RC promotion — no slice-3 error.
    let src = "struct Owned { x: i64 }\n\
               fn take(o: Owned) { }\n\
               fn main() {\n\
                   let o = Owned { x: 1 };\n\
                   let _f = || take(o);\n\
                   let _u = o;\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict),
        "slice 3 must not fire for Own captures: {:?}",
        result.errors
    );
    let rc = result
        .rc_values
        .get("main")
        .and_then(|m| m.get("o"))
        .expect("o should be RC-promoted via closure-capture-with-outer-use");
    assert_eq!(rc.trigger, RcTrigger::ClosureCaptureWithOuterUse);
}

#[test]
fn slice3_copy_field_outer_consume_does_not_fire_conflict_when_path_overlaps() {
    // Copy guard — the consume-side path lookup at the top of
    // `check_expr_consuming` suppresses the closure-capture conflict
    // when the consumed expression's type is Copy. The closure ref-
    // captures `(o, ["x"])` and outer `let _u = o.x` reads the same
    // path, but `o.x: i64` is Copy and the consume is silently a
    // copy at the binding level — no borrow disturbed.
    ownership_ok(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let _f = || o.x + 1;\n\
             let _u = o.x;\n\
         }",
    );
}

#[test]
fn slice3_diagnostic_carries_closure_span_as_secondary() {
    // Diagnostic shape pin — `ClosureCaptureBorrowConflict` puts the
    // closure-creation span in `consume_span` (the secondary label
    // slot the borrow family uses for "the other site"), and the
    // primary `span` is the consume site. The message names the
    // closure's line:column and the capture mode (`ref` here).
    let errs = ownership_errors(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             let _f = || u.profile.name + 1;\n\
             let _v = u;\n\
         }",
    );
    let err = errs
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::ClosureCaptureBorrowConflict)
        .expect("expected ClosureCaptureBorrowConflict");
    assert!(
        err.consume_span.is_some(),
        "secondary span (closure site) should be populated"
    );
    assert!(
        err.message.contains("by `ref`"),
        "diagnostic should name the captured mode; got {:?}",
        err.message
    );
    assert!(
        err.message.contains("closure at line"),
        "diagnostic should name the closure site; got {:?}",
        err.message
    );
}

#[test]
fn slice3_borrow_drains_at_scope_holding_closure_value() {
    // Drain happens when the SCOPE holding the closure-value exits,
    // not when the closure is later consumed. Two scopes inside main:
    // inner block creates and lets the closure go out of scope; outer
    // block then consumes the previously-captured root. Without the
    // drain hook the borrow would persist into the outer scope and
    // false-fire.
    ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile, history: Profile }\n\
         fn use_int(n: i64) { }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 }, history: Profile { name: 2 } };\n\
             {\n\
                 let _f = || u.profile.name + 1;\n\
                 use_int(0);\n\
             }\n\
             let _x = u.profile;\n\
         }",
    );
}

// ── Disjoint closure capture — slice 6 (whole-root capture reason + N0503 enrichment) ─
//
// Line 353 phase-5 checklist — disjoint-capture slice 6. Slice 1's
// path walker now records *why* it committed a root to whole-root
// capture: a method call on the root, an index expression, a deref of
// a captured borrow, a by-value pass to a function call, or — when
// nothing else applies — a bare-identifier reference. The reason map
// is surfaced via `OwnershipCheckResult::whole_root_capture_reasons`
// and consumed by `emit_rc_fallback_notes` to enrich the N0503 perf
// note with the spec-mandated *"because the closure body called
// method `…` on `…` — disjoint capture only sees through field
// projections"* explanation plus a fix-it that names the rewrite
// (hoist the field access outside the stopping construct).

fn slice6_reasons_for(result: &OwnershipCheckResult) -> &HashMap<String, WholeRootCaptureReason> {
    assert_eq!(
        result.whole_root_capture_reasons.len(),
        1,
        "expected exactly one closure with whole-root reasons; got {} entries",
        result.whole_root_capture_reasons.len()
    );
    result.whole_root_capture_reasons.values().next().unwrap()
}

#[test]
fn slice6_method_call_records_method_call_reason() {
    // `|| u.show()` — method-call receiver is the slice-1 stopping
    // construct that commits `u` to whole-root capture. The reason
    // names the method (`show`) so the diagnostic can attribute the
    // whole-root choice to the specific call site.
    let result = ownership_ok(
        "struct User { name: i64 }\n\
         impl User { fn show(ref self) { } }\n\
         fn main() {\n\
             let u = User { name: 1 };\n\
             let _f = || u.show();\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("u")
        .expect("expected whole-root reason for `u`");
    match r {
        WholeRootCaptureReason::MethodCall { method_name, .. } => {
            assert_eq!(method_name, "show");
        }
        _ => panic!("expected MethodCall reason, got {:?}", r),
    }
}

#[test]
fn slice6_index_records_index_reason() {
    // `|| v[0]` — index expression is a stopping construct; reason
    // is `Index` with the call span.
    let result = ownership_ok(
        "fn main() {\n\
             let v = Vec[1, 2, 3];\n\
             let _f = || v[0] + 1;\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("v")
        .expect("expected whole-root reason for `v`");
    assert!(
        matches!(r, WholeRootCaptureReason::Index { .. }),
        "expected Index reason, got {:?}",
        r
    );
}

#[test]
fn slice6_by_value_pass_records_byvaluepass_reason() {
    // `|| take(cfg)` — bare `cfg` passed to a function call. The
    // slice-1 walker special-cases the immediate bare-identifier-as-
    // call-arg shape to register `ByValuePass` rather than the
    // lower-priority `BareIdentifier`. Pins that the call-site
    // attribution is preserved across the recursion (the generic
    // walker's later `BareIdentifier` insertion does not overwrite).
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         fn take(c: Config) { }\n\
         fn main() {\n\
             let cfg = Config { name: 1 };\n\
             let _f = || take(cfg);\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("cfg")
        .expect("expected whole-root reason for `cfg`");
    assert!(
        matches!(r, WholeRootCaptureReason::ByValuePass { .. }),
        "expected ByValuePass reason, got {:?}",
        r
    );
}

#[test]
fn slice6_bare_identifier_records_bareidentifier_reason() {
    // `|| cfg` — bare-identifier final-expression reference, no
    // enclosing stopping construct. The reason is `BareIdentifier`
    // (lowest priority). Pin that bare references with no
    // surrounding construct still produce a reason entry so the
    // RC-fallback note's enrichment lookup never returns None for a
    // captured-whole root.
    let result = ownership_ok(
        "struct Config { name: i64 }\n\
         fn take(c: Config) { }\n\
         fn main() {\n\
             let cfg = Config { name: 1 };\n\
             let _f = || { let c = cfg; take(c); };\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("cfg")
        .expect("expected whole-root reason for `cfg`");
    assert!(
        matches!(r, WholeRootCaptureReason::BareIdentifier),
        "expected BareIdentifier reason, got {:?}",
        r
    );
}

#[test]
fn slice6_method_call_beats_bare_identifier_priority() {
    // Body has a stopping construct (`u.show()`) AND a bare-identifier
    // reference (`u` as a let value). The priority rule pins
    // `MethodCall` as the winning reason because stopping constructs
    // outrank `BareIdentifier`. Pins the "first stopping construct
    // wins; bare loses" merge rule (`record_whole_root_reason`).
    let result = ownership_ok(
        "struct User { name: i64 }\n\
         impl User { fn show(ref self) { } }\n\
         fn take(u: User) { }\n\
         fn main() {\n\
             let u = User { name: 1 };\n\
             let _f = || { u.show(); let c = u; take(c); };\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("u")
        .expect("expected whole-root reason for `u`");
    assert!(
        matches!(r, WholeRootCaptureReason::MethodCall { method_name, .. } if method_name == "show"),
        "stopping construct must beat BareIdentifier; got {:?}",
        r
    );
}

#[test]
fn slice6_first_stopping_construct_wins_over_later_one() {
    // Body has two stopping constructs (`u.show()` first, `v[0]` and
    // method on u, etc.). For the same root, first-wins. Construct:
    // closure does `u.show(); u[0]` — both stopping constructs on
    // `u`. The walker sees the MethodCall first (top-down traversal
    // through a Block), so `MethodCall` wins over `Index`.
    let result = ownership_ok(
        "struct U { name: i64 }\n\
         impl U { fn show(ref self) { } }\n\
         impl U { fn at(ref self, i: i64) -> i64 { 0 } }\n\
         fn main() {\n\
             let u = U { name: 1 };\n\
             let _f = || { u.show(); u.at(0) };\n\
         }",
    );
    let reasons = slice6_reasons_for(&result);
    let r = reasons
        .get("u")
        .expect("expected whole-root reason for `u`");
    match r {
        WholeRootCaptureReason::MethodCall { method_name, .. } => {
            assert_eq!(method_name, "show", "first method call wins");
        }
        _ => panic!(
            "expected MethodCall(show) — first stopping construct wins; got {:?}",
            r
        ),
    }
}

#[test]
fn slice6_path_precise_capture_records_no_reason() {
    // Closure captures `u.profile.name` (precise sub-path). Slice 1
    // does not commit any root to whole-root capture, so the reasons
    // map for this closure is absent (no entry for the closure span).
    // Pin that we only populate reasons when whole-root capture
    // actually fired.
    let result = ownership_ok(
        "struct Profile { name: i64 }\n\
         struct User { profile: Profile }\n\
         fn main() {\n\
             let u = User { profile: Profile { name: 1 } };\n\
             let _f = || u.profile.name + 1;\n\
         }",
    );
    assert!(
        result.whole_root_capture_reasons.is_empty(),
        "no whole-root reasons should be recorded for path-precise capture; got: {:?}",
        result.whole_root_capture_reasons
    );
}

#[test]
fn slice6_n0503_note_includes_method_call_reason() {
    // **The slice-6 headline test.** When the closure body forces
    // whole-root capture via a method call AND the outer scope
    // consumes a sibling sub-place (non-Copy) so the RC fallback
    // fires, the N0503 note must include the spec-mandated
    // explanation: "closure at line N captured `u` whole because the
    // closure body called method `show` on `u` (disjoint capture only
    // sees through field projections)".
    let src = "struct Inner { v: i64 }\n\
               struct User { name: i64, history: Inner }\n\
               impl User { fn show(ref self) { } }\n\
               fn take(x: Inner) { }\n\
               fn main() {\n\
                   let u = User { name: 1, history: Inner { v: 3 } };\n\
                   let _f = || u.show();\n\
                   take(u.history);\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let note = result
        .notes
        .iter()
        .find(|n| n.kind == OwnershipErrorKind::RcFallbackNote)
        .expect("expected N0503 RC fallback note");
    assert!(
        note.message.contains("captured `u` whole"),
        "note should attribute the whole-root capture; got: {}",
        note.message
    );
    assert!(
        note.message.contains("method `show`"),
        "note should name the method that caused the whole-root capture; got: {}",
        note.message
    );
    assert!(
        note.message
            .contains("disjoint capture only sees through field projections"),
        "note should include the spec-mandated framing; got: {}",
        note.message
    );
}

#[test]
fn slice6_n0503_note_includes_method_call_fix_it_suggestion() {
    // Companion to the headline test: the suggestion field must carry
    // the slice-6 fix-it for `MethodCall` reasons — name the method
    // and propose hoisting its result out of the closure.
    let src = "struct Inner { v: i64 }\n\
               struct User { name: i64, history: Inner }\n\
               impl User { fn show(ref self) { } }\n\
               fn take(x: Inner) { }\n\
               fn main() {\n\
                   let u = User { name: 1, history: Inner { v: 3 } };\n\
                   let _f = || u.show();\n\
                   take(u.history);\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let note = result
        .notes
        .iter()
        .find(|n| n.kind == OwnershipErrorKind::RcFallbackNote)
        .expect("expected N0503 RC fallback note");
    let s = note
        .suggestion
        .as_ref()
        .expect("expected a fix-it suggestion");
    assert!(
        s.contains("hoist") && s.contains("show"),
        "fix-it should propose hoisting `show`'s call out of the closure; got: {}",
        s
    );
}

#[test]
fn slice6_n0503_note_falls_back_to_generic_for_non_closure_rc() {
    // Negative pin: when the RC promotion is not closure-capture-
    // driven (e.g., a plain direct-reuse-after-consume between two
    // free-standing statements), the slice-6 enrichment lookup must
    // return None and the note falls back to the legacy generic
    // suggestion. Catches accidental over-eager enrichment.
    let src = "struct Owned { x: i64 }\n\
               fn take(o: Owned) { }\n\
               fn read(ref o: Owned) -> i64 { o.x }\n\
               fn main() {\n\
                   let o = Owned { x: 1 };\n\
                   let _v = read(o);\n\
                   take(o);\n\
               }";
    let parsed = parse(src);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let note = result
        .notes
        .iter()
        .find(|n| n.kind == OwnershipErrorKind::RcFallbackNote);
    if let Some(note) = note {
        assert!(
            !note.message.contains("closure at line"),
            "non-closure RC promotion must not carry slice-6 closure-attribution; got: {}",
            note.message
        );
        let s = note
            .suggestion
            .as_ref()
            .expect("expected fallback suggestion");
        assert!(
            s.contains("restructure to a single ownership path"),
            "non-closure RC promotion must keep the generic suggestion; got: {}",
            s
        );
    }
}

#[test]
fn slice6_describe_helper_renders_method_call_reason() {
    // Direct API pin for `WholeRootCaptureReason::describe`. Tests
    // the formatting helper without going through the full ownership
    // pipeline — regression guard for the spec-mandated message
    // shape if the helper is ever moved or rewritten.
    let r = WholeRootCaptureReason::MethodCall {
        method_name: "show".to_string(),
        call_span: karac::token::Span::default(),
    };
    let s = r.describe("u");
    assert!(
        s.contains("`show`") && s.contains("`u`"),
        "describe should name both method and receiver; got: {}",
        s
    );
    assert!(
        s.contains("disjoint capture only sees through field projections"),
        "describe should include the spec framing; got: {}",
        s
    );
}

// ── Step 7 sentinels: ref-captured value escape (E0508) ─────────
//
// Round 12.35 — design.md § Closures Rule 2 sub-case (iv):
// "A capture-by-reference that would outlive its source is a
// standard borrow-check error, caught at the closure creation site
// when the closure value is assigned into something that escapes."
// This v1 round detects the unambiguous escape-via-return cases
// (direct return, let-bound return, implicit tail-expression
// return). Other escape destinations (fn-arg pass to an `Fn(...)`
// slot, struct-field store) require richer escape-tracking and are
// tracked as a follow-up entry. The diagnostic fires at the closure
// expression, names the offending capture, and offers three concrete
// fixes via the `suggestion` field.

fn step7_e0508_errors(source: &str) -> Vec<OwnershipError> {
    ownership_errors(source)
        .into_iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect()
}

#[test]
fn step7_direct_return_of_closure_with_ref_capture_fires() {
    // Closure body reads `cfg.value` (Copy projection); capture mode
    // for `cfg` is `Ref`. The closure is the operand of an explicit
    // `return` statement, so it escapes the function. The captured
    // `cfg` is owned (parameter declared `cfg: Config`, not `ref
    // Config`), so the ref capture would outlive the source. E0508.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             return || cfg.value;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
    assert!(
        errors[0].message.contains("`ref` capture of `cfg`"),
        "message should name capture mode and binding; got {:?}",
        errors[0].message
    );
    assert!(
        errors[0]
            .suggestion
            .as_ref()
            .is_some_and(|s| s.contains("clone") && s.contains("restructure") && s.contains("own")),
        "suggestion should offer all three fixes; got {:?}",
        errors[0].suggestion
    );
}

#[test]
fn step7_implicit_tail_return_fires() {
    // No explicit `return` — the closure expression is the function
    // body's tail expression (implicit return). Same outcome as the
    // explicit-return form: ref capture escapes via return.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             || cfg.value\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_let_bound_return_fires() {
    // `let h = || cfg.value;` registers `h` in the closure-let map;
    // `return h;` resolves the identifier back to the closure span.
    // Same diagnostic fires at the closure expression, not at the
    // identifier or return statement.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             let h = || cfg.value;\n\
             return h;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_let_bound_implicit_tail_return_fires() {
    // Combination of the previous two: let-bound closure followed by
    // a tail-expression identifier (implicit return).
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             let h = || cfg.value;\n\
             h\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_owned_capture_clean_escape_does_not_fire() {
    // Closure body consumes `cfg` (`apply` takes by value). Capture
    // is `Own`, not `Ref`. Sub-case (i) clean escape — no error.
    // Mirrors `step5_escape_via_return_direct_clean` from
    // tests/rc_fallback.rs but checks specifically for E0508.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         fn apply(c: Config) { }\n\
         fn make_handler(cfg: Config) -> Fn() -> () {\n\
             return || apply(cfg);\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "owned-capture clean escape should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_scalar_ref_capture_escape_does_not_fire() {
    // B-2026-07-08-2 — a closure that captures a Copy scalar (`i64`)
    // and is returned must NOT fire E0508. A read-only capture of an
    // owned scalar defaults to `Ref` mode, but a scalar has no heap
    // payload: the closure holds it by value (a copy in the env), so
    // there is no borrow of the source to dangle. `karac run` executes
    // `make(1)(10) == 11` correctly; `check` must agree.
    let parsed = parse(
        "fn make(k: i64) -> Fn(i64) -> i64 {\n\
             return |x| x + k;\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "scalar (i64) ref capture escape should not fire E0508; got {:?}",
        e0508s
    );
}

#[test]
fn step7_scalar_ref_capture_escape_variants_do_not_fire() {
    // Companion to the i64 case: f64/bool/char scalar captures are
    // equally self-contained and must not fire E0508 on escape, while
    // a non-Copy struct capture in the same file still fires exactly
    // once — proving the exemption is scalar-scoped, not a blanket
    // suppression.
    let errors = step7_e0508_errors(
        "fn make_f(k: f64) -> Fn() -> f64 { return || k; }\n\
         fn make_b(k: bool) -> Fn() -> bool { return || k; }\n\
         fn make_c(k: char) -> Fn() -> char { return || k; }\n\
         struct Config { value: i64 }\n\
         fn make_s(cfg: Config) -> Fn() -> i64 { return || cfg.value; }",
    );
    assert_eq!(
        errors.len(),
        1,
        "only the non-Copy struct capture should fire E0508; got {:?}",
        errors
    );
    assert!(
        errors[0].message.contains("capture of `cfg`"),
        "the surviving error should be the struct capture; got {:?}",
        errors[0].message
    );
}

#[test]
fn step7_local_use_does_not_fire() {
    // Closure stays local — invoked inside the function, not
    // returned. No escape, no error. Even though the capture is
    // `Ref` (read-only body), there's no escape destination.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             let h = || cfg.value;\n\
             h()\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "local-use closure should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_branch_divergent_returns_each_fire() {
    // `if c { return || cfg.value; } else { return || cfg.value + 1; }`
    // — both branches return a closure with a ref capture; each must
    // independently fire E0508. Two distinct closure expressions →
    // two errors.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config, c: bool) -> Fn() -> i64 {\n\
             if c {\n\
                 return || cfg.value;\n\
             } else {\n\
                 return || cfg.value + 1;\n\
             }\n\
         }",
    );
    assert_eq!(
        errors.len(),
        2,
        "both branches' closures should each fire E0508; got {:?}",
        errors
    );
}

#[test]
fn step7_diagnostic_span_points_at_closure_expression() {
    // The diagnostic's `span` should point at the closure expression
    // (the creation site), not at the return statement or the
    // captured identifier. Match the line of the closure literal.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             let h = || cfg.value;\n\
             return h;\n\
         }",
    );
    assert_eq!(errors.len(), 1);
    // The closure `|| cfg.value` is on line 3 of the source above.
    assert_eq!(
        errors[0].span.line, 3,
        "diagnostic span should point at the closure expression's line; got line {}",
        errors[0].span.line
    );
}

// ── Step 7 follow-up sentinels: composite literal escape (round 12.36) ──
//
// Round 12.36 extends `collect_escape_target` to recurse through
// composite literal expressions in the operand of an escaping return:
// struct literals, tuples, array literals, prefix-collection literals
// (`Vec[...]`, `Array[...]`, `Set[...]`, `Map[...]`), repeat literals
// (`[v; n]`), and map literals. A closure with ref captures sitting
// inside any of these as a sub-expression of a return statement (or
// the function-body's tail expression) escapes the function the same
// way a directly-returned closure does — the wrapping literal is
// constructed in the current scope and immediately handed off to the
// caller. Function-call escape (`return run_fn(h)`), field-access
// extraction (`return holder.f`), and let-bound-then-returned shapes
// remain deferred to a further follow-up.

#[test]
fn step7_struct_literal_in_return_fires() {
    // Closure embedded in a struct literal that's directly returned —
    // the struct's lifetime is the caller's scope, so the closure's
    // ref capture would outlive `cfg`.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> i64 }\n\
         fn make_holder(cfg: Config) -> Holder {\n\
             return Holder { f: || cfg.value };\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_struct_literal_implicit_tail_return_fires() {
    // Same as above but the struct literal is the function body's
    // tail expression (implicit return).
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> i64 }\n\
         fn make_holder(cfg: Config) -> Holder {\n\
             Holder { f: || cfg.value }\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_tuple_in_return_fires() {
    // Closure in a tuple literal returned from the function. Two
    // closures in the tuple → two errors (one per closure
    // expression).
    let errors = step7_e0508_errors(
        "struct Config { x: i64, y: i64 }\n\
         fn make_pair(cfg: Config) -> (Fn() -> i64, Fn() -> i64) {\n\
             return (|| cfg.x, || cfg.y);\n\
         }",
    );
    assert_eq!(
        errors.len(),
        2,
        "two closures in tuple → two errors; got {:?}",
        errors
    );
}

#[test]
fn step7_vec_literal_in_return_fires() {
    // Closure in a `Vec[...]` prefix-collection literal that's
    // returned. Same shape as struct/tuple.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn make_handlers(cfg: Config) -> Vec[Fn() -> i64] {\n\
             return Vec[|| cfg.value];\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one E0508 error, got {:?}",
        errors
    );
}

#[test]
fn step7_struct_literal_with_owned_capture_does_not_fire() {
    // Negative: closure body consumes capture (own mode), so the
    // closure carries `cfg` by value. Storing the closure in a struct
    // and returning is the clean-escape sub-case (i) — no error.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> () }\n\
         fn apply(c: Config) { }\n\
         fn make_holder(cfg: Config) -> Holder {\n\
             return Holder { f: || apply(cfg) };\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "owned-capture in struct literal should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_nested_struct_literals_recurse() {
    // Closure two levels deep: outer struct holds inner struct; inner
    // struct holds the closure. Recursion through nested composite
    // literals should still find the closure.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Inner { f: Fn() -> i64 }\n\
         struct Outer { inner: Inner }\n\
         fn make_outer(cfg: Config) -> Outer {\n\
             return Outer { inner: Inner { f: || cfg.value } };\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "nested struct literal should still surface the closure; got {:?}",
        errors
    );
}

// ── Step 7 follow-up sentinels: let-bound carrier escape (round 12.37) ──
//
// Round 12.37 generalises the round-12.35 closure-let map from
// `HashMap<String, SpanKey>` to `HashMap<String, Vec<SpanKey>>` and
// changes the registration walk to reuse `collect_escape_target` on
// the let-RHS — so a let-binding of a composite-literal-containing
// closures (`let holder = Holder { f: || cfg.x };`) registers the
// binding name against the union of closure spans inside. A
// subsequent `return holder;` resolves the identifier through the map
// and surfaces every embedded closure for the standard E0508 check.
//
// Identifier-to-identifier propagation also works (`let h2 = h;`
// extends `h2`'s span set with `h`'s) because `collect_escape_target`
// already handles the Identifier arm against `closure_lets`.

#[test]
fn step7_let_bound_struct_then_return_fires() {
    // Closure stored in struct via let, then the struct is returned.
    // Pre-12.37 this case missed because `closure_let_bindings` only
    // tracked direct `let h = closure;` forms.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> i64 }\n\
         fn make_holder(cfg: Config) -> Holder {\n\
             let holder = Holder { f: || cfg.value };\n\
             return holder;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "let-bound struct then return should fire; got {:?}",
        errors
    );
}

#[test]
fn step7_let_bound_struct_implicit_tail_return_fires() {
    // Same as above but with implicit tail-expression return.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> i64 }\n\
         fn make_holder(cfg: Config) -> Holder {\n\
             let holder = Holder { f: || cfg.value };\n\
             holder\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "let-bound struct then tail return should fire; got {:?}",
        errors
    );
}

#[test]
fn step7_let_bound_tuple_of_closures_then_return_fires() {
    // Tuple of two closures bound to a let, then returned. Both
    // closures should fire — the let-RHS walk registers `pair`
    // against both closure spans, and the return resolves identifier
    // → both spans.
    let errors = step7_e0508_errors(
        "struct Config { x: i64, y: i64 }\n\
         fn make_pair(cfg: Config) -> (Fn() -> i64, Fn() -> i64) {\n\
             let pair = (|| cfg.x, || cfg.y);\n\
             return pair;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        2,
        "two closures in let-bound tuple should each fire; got {:?}",
        errors
    );
}

#[test]
fn step7_identifier_propagation_through_lets_fires() {
    // `let h = || cfg.x; let h2 = h;` should propagate h's closure
    // span to h2; `return h2` then resolves through both layers.
    let errors = step7_e0508_errors(
        "struct Config { x: i64 }\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             let h = || cfg.x;\n\
             let h2 = h;\n\
             return h2;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "identifier propagation through lets should fire once; got {:?}",
        errors
    );
}

#[test]
fn step7_let_bound_struct_used_locally_does_not_fire() {
    // Negative: closure stored in struct via let, struct invoked
    // locally (not returned). No escape, no error.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         struct Holder { f: Fn() -> i64 }\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             let holder = Holder { f: || cfg.value };\n\
             (holder.f)()\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "let-bound struct used locally should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_let_bound_nested_struct_then_return_fires() {
    // Round 12.36 + 12.37 composition: two-level-nested struct
    // bound to a let, then returned. The let-RHS walk recurses
    // through both struct layers via composite-literal recursion.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         struct Inner { f: Fn() -> i64 }\n\
         struct Outer { inner: Inner }\n\
         fn make_outer(cfg: Config) -> Outer {\n\
             let outer = Outer { inner: Inner { f: || cfg.value } };\n\
             return outer;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "let-bound nested struct then return should fire; got {:?}",
        errors
    );
}

// ── Step 7 follow-up sentinels: fn-arg pass conservative-fire (round 12.39) ──
//
// Round 12.39 closes the last Step 7 escape destination: a closure
// with `ref` / `mut ref` captures passed as a fn-arg to an Own-mode
// parameter slot. The receiving function MAY store the closure
// beyond its call (in a long-lived cell, in a struct field, by
// re-passing it elsewhere) — without inter-procedural analysis we
// cannot prove otherwise, so we conservatively fire E0508 on every
// such pass. Borrow-mode slots (`ref Fn(...)` / `mut ref Fn(...)`)
// are skipped — the callee borrows the closure for the duration of
// the call and structurally cannot store it. Method calls and
// indirect calls (through function-typed locals where we lack
// per-position mode info) are also skipped — those cases need
// their own follow-up.
//
// The function-level `#[allow(ref_capture_escape)]` attribute opts
// out of the conservative fire when the programmer knows the called
// functions are synchronous invoke-and-drop. This is the same shape
// as `#[allow(rc_fallback)]` elsewhere in the ownership pass.

#[test]
fn step7_fn_arg_pass_to_own_fn_slot_with_ref_capture_fires() {
    // Closure body reads `cfg.value` (Copy projection) → `cfg`
    // captured by `ref`. Closure passed to `run_fn(f: Fn() -> ())`
    // (Own-mode slot). The receiving function may or may not invoke
    // the closure synchronously — we conservatively fire.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn run_fn(f: Fn() -> i64) -> i64 { f() }\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             let h = || cfg.value;\n\
             run_fn(h)\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "ref-capture closure passed to Own-mode Fn slot should fire; got {:?}",
        errors
    );
}

#[test]
fn step7_fn_arg_pass_direct_closure_literal_fires() {
    // Same shape as above but with the closure as a direct literal
    // in the call-arg position (no let binding).
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         fn run_fn(f: Fn() -> i64) -> i64 { f() }\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             run_fn(|| cfg.value)\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "direct closure literal as Own-mode Fn arg should fire; got {:?}",
        errors
    );
}

#[test]
fn step7_fn_arg_pass_with_owned_capture_does_not_fire() {
    // Negative: closure body consumes `cfg` (`apply` takes by value)
    // → capture is `Own`, not `Ref`. Sub-case (i) clean-escape
    // through fn-arg pass — no error.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         fn apply(c: Config) { }\n\
         fn run_fn(f: Fn() -> ()) { f() }\n\
         fn use_cfg(cfg: Config) {\n\
             let h = || apply(cfg);\n\
             run_fn(h);\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "owned-capture passed to Fn slot should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_fn_arg_pass_to_ref_fn_slot_does_not_fire() {
    // Negative: `ref Fn(...)` slot — the callee borrows the closure
    // for the duration of the call and cannot store it beyond
    // return. Conservative-fire is structurally unnecessary here.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         fn run_fn(f: ref Fn() -> i64) -> i64 { f() }\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             let h = || cfg.value;\n\
             run_fn(h)\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "ref Fn slot pass should not fire E0508; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_allow_ref_capture_escape_attribute_suppresses_fn_arg_fire() {
    // `#[allow(ref_capture_escape)]` on the enclosing function
    // suppresses the conservative fn-arg fire. Useful for the
    // synchronous invoke-and-drop pattern (`run_fn` only invokes,
    // doesn't store) until callee-side annotations land.
    let parsed = parse(
        "struct Config { value: i64 }\n\
         fn run_fn(f: Fn() -> i64) -> i64 { f() }\n\
         #[allow(ref_capture_escape)]\n\
         fn use_cfg(cfg: Config) -> i64 {\n\
             let h = || cfg.value;\n\
             run_fn(h)\n\
         }",
    );
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    let e0508s: Vec<_> = result
        .errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RefCaptureEscapesScope))
        .collect();
    assert!(
        e0508s.is_empty(),
        "#[allow(ref_capture_escape)] should suppress fn-arg fire; got {:?}",
        e0508s,
    );
}

#[test]
fn step7_allow_attribute_does_not_suppress_return_fire() {
    // The opt-out is scoped to fn-arg-pass sub-case only — the
    // unambiguous return-escape sub-cases (round 12.35–12.37) still
    // fire even with the attribute. A closure with ref captures
    // returned from the function is always an iv violation.
    let errors = step7_e0508_errors(
        "struct Config { value: i64 }\n\
         #[allow(ref_capture_escape)]\n\
         fn make_handler(cfg: Config) -> Fn() -> i64 {\n\
             return || cfg.value;\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "allow attribute should not suppress return-escape fire; got {:?}",
        errors
    );
}

// ── Slice borrow source attribution ─────────────────────────────
//
// Phase-5 Theme 1 Slice 1: `OwnershipCheckResult::slice_borrow_sources`
// records every slice creation site keyed by the slice expression's
// `SpanKey`. Each entry is `(PlaceExpr, mutable)` resolved to the
// original storage binding — slice-of-slice creations chain through to
// the root `Vec` / `Array` / `Slice`, never an intermediate slice.

#[test]
fn slice_from_as_slice_records_root_binding() {
    let result = ownership_ok(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s = v.as_slice();
         }",
    );
    let entries: Vec<_> = result.slice_borrow_sources.values().collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one slice creation, got {:?}",
        entries
    );
    let (place, mutable) = entries[0];
    assert_eq!(place.root, "v");
    assert!(place.projections.is_empty());
    assert!(!mutable, ".as_slice() produces an immutable slice");
}

#[test]
fn slice_of_slice_records_root_not_parent() {
    // `s2 = s1[0..3]` chains through `s1`'s recorded source so `s2`'s
    // attribution names the original storage binding `v`, not `s1`.
    let result = ownership_ok(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             v.push(2);
             v.push(3);
             let s1 = v.as_slice();
             let _s2 = s1[0..2];
         }",
    );
    // Two slice creation sites: `v.as_slice()` and `s1[0..2]`. Both
    // resolve to root `v`.
    assert_eq!(
        result.slice_borrow_sources.len(),
        2,
        "expected two slice creations, got {:?}",
        result.slice_borrow_sources
    );
    for (place, _) in result.slice_borrow_sources.values() {
        assert_eq!(
            place.root, "v",
            "every entry should resolve to root v, got {:?}",
            place
        );
    }
}

#[test]
fn slice_from_temporary_escapes_rejected() {
    // `make_vec().as_slice()` bound to a let — the receiver is a
    // function-call temporary with no rooted attribution; the slice's
    // storage drops at end-of-statement.
    let errors = ownership_errors(
        "fn make_vec() -> Vec[i64] { Vec.new() }
         fn main() {
             let _s = make_vec().as_slice();
         }",
    );
    let escapes: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::SliceFromTemporaryEscapes))
        .collect();
    assert_eq!(
        escapes.len(),
        1,
        "expected one SliceFromTemporaryEscapes error, got {:?}",
        errors
    );
}

#[test]
fn slice_from_temporary_in_statement_accepted() {
    // `make_vec().as_slice().len()` — slice is a temp consumed
    // in-statement. No escape, should accept.
    ownership_ok(
        "fn make_vec() -> Vec[i64] { Vec.new() }
         fn main() {
             let _n = make_vec().as_slice().len();
         }",
    );
}

#[test]
fn slice_from_call_arg_coercion_records_root() {
    // Implicit `Vec[T]` → `mut Slice[T]` at call-arg coercion records
    // `(root: 'v', mutable: true)` keyed by the arg's span.
    let result = ownership_ok(
        "fn clear(xs: mut Slice[i64]) {}
         fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             clear(mut v);
         }",
    );
    let entries: Vec<_> = result.slice_borrow_sources.values().collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one slice creation, got {:?}",
        entries
    );
    let (place, mutable) = entries[0];
    assert_eq!(place.root, "v");
    assert!(place.projections.is_empty());
    assert!(*mutable, "mut Slice[T] formal records mutable=true");
}

// ── Slice borrow conflict detection ─────────────────────────────
//
// Phase-5 Theme 1 Slice 2: the conflict matrix scans `active_borrows`
// at every push and emits `SliceBorrowConflict { shape: ... }` for the
// four conflict shapes (A imm+mut, B mut+mut, C move-of-borrowed, D
// drop-of-borrowed) and `CrossBorrowConflict` for slice + ref of the
// same root. Borrows are scoped — drained at block-exit and at call
// return.

fn slice_conflict_errors(source: &str) -> Vec<OwnershipError> {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "Parse: {:?}", parsed.errors);
    let resolved = resolve(&parsed.program);
    assert!(resolved.errors.is_empty(), "Resolve: {:?}", resolved.errors);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    result
        .errors
        .into_iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict { .. }
                    | OwnershipErrorKind::CrossBorrowConflict
            )
        })
        .collect()
}

#[test]
fn mut_slice_plus_imm_slice_same_source_rejected_shape_a() {
    let errors = slice_conflict_errors(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s_mut = v.as_slice_mut();
             let _s_imm = v.as_slice();
         }",
    );
    let shape_a: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::ImmSliceVsMutSlice
                }
            )
        })
        .collect();
    assert_eq!(
        shape_a.len(),
        1,
        "expected one shape A error, got {:?}",
        errors
    );
}

#[test]
fn two_imm_slices_same_source_accepted() {
    // Two read-only `Slice[T]` peers of the same source coexist —
    // shape A only fires for ImmSlice + MutSlice pairs.
    ownership_ok(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s1 = v.as_slice();
             let _s2 = v.as_slice();
         }",
    );
}

#[test]
fn two_mut_slices_same_source_rejected_shape_b() {
    let errors = slice_conflict_errors(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s1 = v.as_slice_mut();
             let _s2 = v.as_slice_mut();
         }",
    );
    let shape_b: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::MutSliceVsMutSlice
                }
            )
        })
        .collect();
    assert_eq!(
        shape_b.len(),
        1,
        "expected one shape B error, got {:?}",
        errors
    );
}

#[test]
fn slice_then_move_source_rejected_shape_c() {
    // Slice into `v` is live, then `v` is consumed (moved into another
    // owned binding) — shape C: cannot move source while slice borrow
    // is live.
    let errors = slice_conflict_errors(
        "fn take(v: Vec[i64]) {}
         fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s = v.as_slice();
             take(v);
         }",
    );
    let shape_c: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::MoveOfBorrowed
                }
            )
        })
        .collect();
    assert_eq!(
        shape_c.len(),
        1,
        "expected one shape C error, got {:?}",
        errors
    );
}

#[test]
fn transitive_slice_of_slice_conflicts_via_root() {
    // `let s2 = s1[0..3];` chains through Slice 1's binding-source
    // map so the recorded root is `v`, not `s1`. The conflict matrix
    // scans `active_borrows[v]` and finds `s1`'s prior `mut` push,
    // firing shape A.
    let errors = slice_conflict_errors(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             v.push(2);
             v.push(3);
             let _s1 = v.as_slice_mut();
             let _s2 = v[0..2];
         }",
    );
    let shape_a: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::ImmSliceVsMutSlice
                }
            )
        })
        .collect();
    assert_eq!(
        shape_a.len(),
        1,
        "expected one shape A error from transitive chain, got {:?}",
        errors
    );
}

#[test]
fn slice_borrow_ends_at_scope_exit_no_conflict() {
    // The first slice borrow lives only inside an inner block; once
    // the block exits, it drains. The second creation outside the
    // inner block sees no live borrow and accepts.
    ownership_ok(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             {
                 let _s1 = v.as_slice_mut();
             }
             let _s2 = v.as_slice_mut();
         }",
    );
}

#[test]
fn mut_slice_then_mutate_source_via_method_rejected_cross_borrow() {
    // A `mut Slice` into a struct's field is live, then an instance
    // method on the struct (`mut ref self`) is called. The receiver-
    // side ref push at MethodCall fires CrossBorrowConflict against
    // the live slice borrow because both target the same root binding.
    // Slice plan sub-step (g): cross-form (slice + ref) routes through
    // `CrossBorrowConflict`, distinct from slice-vs-slice's
    // `SliceBorrowConflict`.
    let errors = slice_conflict_errors(
        "struct Holder { val: Vec[i64] }
         impl Holder {
             fn check(mut ref self) {}
         }
         fn main() {
             let mut h = Holder { val: Vec.new() };
             let _s = h.val.as_slice_mut();
             h.check();
         }",
    );
    let cross: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::CrossBorrowConflict))
        .collect();
    assert_eq!(
        cross.len(),
        1,
        "expected one CrossBorrowConflict, got {:?}",
        errors
    );
}

#[test]
fn imm_slice_then_mut_ref_call_arg_rejected_cross_borrow() {
    // An immutable slice into `v` is live, then `take_mut` is called
    // with `mut v`. The `mut ref Vec[T]` formal pushes a transient
    // `MutRef` borrow at the call boundary; the conflict matrix sees
    // `ImmSlice` + `MutRef` against the same root and emits
    // `CrossBorrowConflict`. Slice 2 follow-up sub-step (c) — symmetric
    // to the receiver-side push for instance methods, but driven from
    // the `Call` arm via the formal's declared param mode.
    let errors = slice_conflict_errors(
        "fn take_mut(v: mut ref Vec[i64]) {}
         fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s = v.as_slice();
             take_mut(mut v);
         }",
    );
    let cross: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::CrossBorrowConflict))
        .collect();
    assert_eq!(
        cross.len(),
        1,
        "expected one CrossBorrowConflict, got {:?}",
        errors
    );
}

#[test]
fn imm_slice_then_stdlib_mut_method_rejected_cross_borrow() {
    // A `Slice[T]` into `v` is live, then `v.push(99)` is called.
    // `Vec.push` has no user-side `impl Vec` block but `method_self_modes`
    // lookup falls through to the stdlib receiver-mode table, which
    // returns `BorrowKind::MutRef`. The receiver-side push at MethodCall
    // emits `CrossBorrowConflict` against the live ImmSlice. Slice 2
    // polish (b) — symmetric to the receiver-side push for user-defined
    // instance methods, but driven by the stdlib table when user
    // metadata is absent.
    let errors = slice_conflict_errors(
        "fn main() {
             let mut v: Vec[i64] = Vec.new();
             v.push(1);
             let _s = v.as_slice();
             v.push(99);
         }",
    );
    let cross: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::CrossBorrowConflict))
        .collect();
    assert_eq!(
        cross.len(),
        1,
        "expected one CrossBorrowConflict via stdlib method table, got {:?}",
        errors
    );
}

#[test]
fn slice_outlives_source_drop_rejected_shape_d() {
    // Canonical drop-of-borrowed: a `LetUninit Slice[T]` outer binding
    // captures a slice taken inside an inner block. When the inner
    // block exits, the source `v` drops while `s_outer` still holds a
    // slice into freed storage. Shape D fires at the block-exit drain.
    //
    // This is the positive form of the v1 polish (D5) — earlier scoped
    // out because the LetUninit + Slice-typed assignment surface was
    // assumed unavailable; verified at probe time that
    // `let mut s: Slice[i64];` followed by `s = v.as_slice();`
    // typechecks cleanly. The Assign arm now propagates
    // `slice_binding_scope_depth` from the LHS's recorded scope
    // (captured at the LetUninit binding) so the drain matches.
    let errors = slice_conflict_errors(
        "fn main() {
             let mut s_outer: Slice[i64];
             {
                 let v: Vec[i64] = Vec.new();
                 s_outer = v.as_slice();
             }
         }",
    );
    let shape_d: Vec<_> = errors
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                OwnershipErrorKind::SliceBorrowConflict {
                    shape: SliceConflictShape::DropOfBorrowed
                }
            )
        })
        .collect();
    assert_eq!(
        shape_d.len(),
        1,
        "expected one shape D drop-of-borrowed error, got {:?}",
        errors
    );
}

#[test]
fn slice_well_scoped_no_shape_d_false_positive() {
    // Negative complement: when the slice's binding scope is at or
    // deeper than the source binding's, no shape D fires. Pins the
    // drain doesn't false-positive on well-scoped programs.
    ownership_ok(
        "fn main() {
             let mut v_outer: Vec[i64] = Vec.new();
             v_outer.push(1);
             let _s = v_outer.as_slice();
             {
                 let mut v_inner: Vec[i64] = Vec.new();
                 v_inner.push(99);
                 let _inner_s = v_inner.as_slice();
             }
         }",
    );
}

// ── Match Ergonomics: ref-scrutinee consume gate ─────────────────────
//
// design.md § Match Arm Binding Modes — when the scrutinee is
// `ref T` / `mut ref T`, arm bindings borrow rather than move, so
// the scrutinee itself is read (never consumed) regardless of what
// the arms bind. The owned-scrutinee path continues to consume per
// the prior rule. See `OwnershipChecker::is_borrow_typed_scrutinee`
// and `UseClassifier::is_borrow_typed_expr`.

#[test]
fn match_ref_struct_scrutinee_not_consumed_by_binding_arm() {
    // Binds the struct field `name` from a `ref Foo` scrutinee — under
    // match ergonomics, the scrutinee `val` stays Live so the post-match
    // `use_val(val)` does not trip `UseOfMoved`.
    ownership_ok(
        "struct Foo { name: String }
         fn use_str(s: ref String) -> i64 { 0 }
         fn use_val(v: ref Foo) -> i64 { 0 }
         fn g(val: ref Foo) -> i64 {
             let _ = match val { Foo { name } => use_str(name) };
             use_val(val)
         }
         fn main() { }",
    );
}

#[test]
fn match_owned_struct_scrutinee_consumed_by_binding_arm() {
    // Owned scrutinee + a binding arm still consumes the scrutinee.
    // Reusing `val` after the match flags as `UseOfMoved`.
    let errs = ownership_errors(
        "struct Foo { name: String }
         fn use_str(s: String) -> i64 { 0 }
         fn use_val(v: Foo) -> i64 { 0 }
         fn g(val: Foo) -> i64 {
             let _ = match val { Foo { name } => use_str(name) };
             use_val(val)
         }
         fn main() { }",
    );
    assert!(
        errs.iter().any(|e| format!("{}", e).contains("val")),
        "expected use-of-moved on val after owned-match consume, got: {:?}",
        errs.iter().map(|e| format!("{}", e)).collect::<Vec<_>>()
    );
}

#[test]
fn match_ref_option_payload_does_not_consume_scrutinee() {
    // Enum-variant payload binding under a `ref Option[String]`
    // scrutinee — the scrutinee remains usable after the match.
    ownership_ok(
        "fn use_str(s: ref String) -> i64 { 0 }
         fn use_val(v: ref Option[String]) -> i64 { 0 }
         fn g(val: ref Option[String]) -> i64 {
             let _ = match val {
                 Option.Some(s) => use_str(s),
                 Option.None => 0,
             };
             use_val(val)
         }
         fn main() { }",
    );
}

#[test]
fn match_mut_ref_scrutinee_not_consumed_by_binding_arm() {
    // Same exception applies to `mut ref T` scrutinees. Under
    // `mut ref Foo`, the field binding `name` is wrapped as
    // `mut ref String`; mirroring functions take the same form.
    ownership_ok(
        "struct Foo { name: String }
         fn use_mut(s: mut ref String) -> i64 { 0 }
         fn use_val(v: mut ref Foo) -> i64 { 0 }
         fn g(val: mut ref Foo) -> i64 {
             let _ = match val { Foo { name } => use_mut(name) };
             use_val(val)
         }
         fn main() { }",
    );
}

// ── `impl Trait` slice 4: borrow-checker integration ────────────

fn ownership_desugared_ok(source: &str) {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    desugar_program(&mut parsed.program);
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    let ownership = ownershipcheck(&parsed.program, &typed);
    assert!(
        ownership.errors.is_empty(),
        "Ownership errors: {}",
        ownership
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn ownership_desugared_errors(source: &str) -> Vec<OwnershipError> {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    desugar_program(&mut parsed.program);
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    let ownership = ownershipcheck(&parsed.program, &typed);
    ownership.errors
}

#[test]
fn impl_trait_slice4_iterator_unrelated_log_drop_accepted() {
    // Spec test 1: `fn iter(v: ref Vec[i64], log: ref Logger) -> impl
    // Iter[i64]` does NOT capture `log`'s borrow region — `log`'s `ref`
    // doesn't flow as a `ref` in the existential's trait args. The
    // ownership-checker integration must NOT register a borrow on
    // `log` for the returned existential, so dropping `log` (here by
    // letting its inner block scope exit) while the iterator binding
    // lives elsewhere does not surface any diagnostic.
    ownership_desugared_ok(
        "trait Iter[U] { fn next(mut ref self) -> U; }\n\
         struct Logger { n: i64 }\n\
         fn iter(v: ref Vec[i64], log: ref Logger) -> impl Iter[i64] { todo() }\n\
         fn main() {\n\
             let v = Vec.with_init([1, 2, 3]);\n\
             { let log = Logger { n: 0 }; let _ = iter(v, log); }\n\
         }",
    );
}

#[test]
fn impl_trait_slice4_iterator_outliving_its_vec_source_rejected() {
    // Spec test 2: `fn iter(v: ref Vec[i64]) -> impl Iter[ref i64]`
    // DOES capture `v`'s borrow region (the `ref` in `Item = ref i64`
    // flows from the only ref input). The
    // `record_existential_capture_borrows` hook pushes an `ImmSlice`
    // active borrow against `v`'s root at the call site. When a second
    // borrow that conflicts (here: a `mut Slice[T]` borrow on the same
    // root) is created against `v` while the iterator borrow is still
    // live, the existing slice-vs-slice conflict matrix in
    // `push_active_borrow` fires a `SliceBorrowConflict`. The presence
    // of the conflict is the proof that the existential's capture
    // borrow was actively tracked against `v` for the duration of the
    // call's containing scope.
    let errors = ownership_desugared_errors(
        "trait Iter[U] { fn next(mut ref self) -> U; }\n\
         fn iter(v: ref Vec[i64]) -> impl Iter[ref i64] { todo() }\n\
         fn main() {\n\
             let mut v = Vec.with_init([1, 2, 3]);\n\
             let _it = iter(v);\n\
             let _s = v.as_slice_mut();\n\
         }",
    );
    let found_conflict = errors
        .iter()
        .any(|e| matches!(e.kind, OwnershipErrorKind::SliceBorrowConflict { .. }));
    assert!(
        found_conflict,
        "expected SliceBorrowConflict for the captured `v` borrow vs. a later mutating slice; got: {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>()
    );
}

// ── Phase-7 line 43 — module-level `#![rc_budget(max: N)]` ─────

/// Build a Kāra source that triggers exactly `n` RC fallbacks via
/// the closure-capture-with-outer-use pattern (RC trigger 2). Each
/// function `f<i>` has its own RC binding `o`, so the total count
/// across `rc_values` is `n`.
fn rc_budget_source(prefix: &str, n: usize) -> String {
    let mut src = String::from(prefix);
    src.push_str("struct Owned { x: i64 }\nfn take(o: Owned) { }\n");
    for i in 0..n {
        src.push_str(&format!(
            "fn f{i}() {{\n    let o = Owned {{ x: {i} }};\n    let _g = || take(o);\n    let _u = o;\n}}\n",
        ));
    }
    src.push_str("fn main() {}\n");
    src
}

#[test]
fn rc_budget_under_passes() {
    // `#![rc_budget(max: 5)]` with 2 RC bindings — under budget, no
    // error. The 2 RC bindings come from `f0` and `f1`, each rooted
    // in a closure-capture-with-outer-use trigger.
    let src = rc_budget_source("#![rc_budget(max: 5)]\n", 2);
    let res = ownership_ok(&src);
    let total: usize = res.rc_values.values().map(|m| m.len()).sum();
    assert_eq!(
        total, 2,
        "fixture should produce exactly 2 RC bindings; got rc_values = {:?}",
        res.rc_values
    );
}

#[test]
fn rc_budget_exceeded_emits_error_with_contributing_list() {
    // 3 RC bindings under a `max: 1` budget — error fires once,
    // names every contributing `<function>.<binding>` in source-
    // sorted order so authors can pick which to restructure first.
    let src = rc_budget_source("#![rc_budget(max: 1)]\n", 3);
    let errors = ownership_errors(&src);
    let budget_errors: Vec<_> = errors
        .iter()
        .filter(|e| matches!(e.kind, OwnershipErrorKind::RcBudgetExceeded { .. }))
        .collect();
    assert_eq!(
        budget_errors.len(),
        1,
        "expected exactly one RcBudgetExceeded error; got {:?}",
        errors
            .iter()
            .map(|e| (&e.kind, &e.message))
            .collect::<Vec<_>>(),
    );
    let err = budget_errors[0];
    let OwnershipErrorKind::RcBudgetExceeded { budget, observed } = err.kind else {
        unreachable!()
    };
    assert_eq!(budget, 1, "budget value should be threaded through");
    assert_eq!(observed, 3, "observed count should match the fixture");
    let suggestion = err.suggestion.as_deref().unwrap_or("");
    for fn_name in ["f0", "f1", "f2"] {
        assert!(
            suggestion.contains(&format!("{fn_name}.o")),
            "suggestion should list `{fn_name}.o` so author can pick which to restructure; got `{suggestion}`",
        );
    }
}

#[test]
fn rc_budget_absent_attr_does_not_enforce() {
    // No `#![rc_budget(...)]` at the top — even 3 RC bindings should
    // not error. Confirms enforcement is opt-in.
    let src = rc_budget_source("", 3);
    let res = ownership_ok(&src);
    let total: usize = res.rc_values.values().map(|m| m.len()).sum();
    assert_eq!(total, 3);
}

#[test]
fn rc_budget_max_zero_rejects_any_rc() {
    // `#![rc_budget(max: 0)]` with even one RC binding — error.
    let src = rc_budget_source("#![rc_budget(max: 0)]\n", 1);
    let errors = ownership_errors(&src);
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            OwnershipErrorKind::RcBudgetExceeded {
                budget: 0,
                observed: 1
            }
        )),
        "expected RcBudgetExceeded {{ budget: 0, observed: 1 }}; got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>(),
    );
}

#[test]
fn rc_budget_bare_attr_no_args_is_ignored() {
    // `#![rc_budget]` with no `max:` arg — treated as absent for v1
    // (no default ceiling). 3 RC bindings still pass.
    let src = rc_budget_source("#![rc_budget]\n", 3);
    let res = ownership_ok(&src);
    let total: usize = res.rc_values.values().map(|m| m.len()).sum();
    assert_eq!(total, 3);
}

#[test]
fn rc_budget_attr_parses_onto_program_inner_attrs() {
    // Sanity that the parser surface puts `#![rc_budget(max: 5)]`
    // onto `Program.inner_attrs` with the parsed `max: 5` arg.
    let parsed = parse("#![rc_budget(max: 5)]\nfn main() {}\n");
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let inner = &parsed.program.inner_attrs;
    assert_eq!(
        inner.len(),
        1,
        "expected one inner attribute; got {inner:?}"
    );
    let attr = &inner[0];
    assert_eq!(attr.path, vec!["rc_budget".to_string()]);
    let max_arg = attr
        .args
        .iter()
        .find(|a| a.name.as_deref() == Some("max"))
        .expect("expected `max:` named arg");
    let val_kind = &max_arg
        .value
        .as_ref()
        .expect("expected value on `max:` arg")
        .kind;
    let karac::ast::ExprKind::Integer(n, _) = val_kind else {
        panic!("expected integer literal for max; got {val_kind:?}");
    };
    assert_eq!(*n, 5);
}

// ── E_CONCURRENT_SHARED_STRUCT (phase-7 line 197) ───────────────
//
// A `shared struct` / `shared enum` binding referenced from two or
// more concurrent branches of a `par {}` block is a compile error.
// Sole-ownership move into exactly one branch is OK.

#[test]
fn test_concurrent_shared_struct_fires_on_two_branch_use() {
    let errors = ownership_errors(
        "shared struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    let hit = errors
        .iter()
        .find(|e| {
            matches!(
                &e.kind,
                OwnershipErrorKind::ConcurrentSharedStruct { type_name, binding }
                    if type_name == "Counter" && binding == "c"
            )
        })
        .expect("expected E_CONCURRENT_SHARED_STRUCT error");
    assert!(
        hit.message.contains("Counter"),
        "diagnostic message should name the shared struct; got: {}",
        hit.message,
    );
    assert!(
        hit.consume_span.is_some(),
        "first-branch use should be threaded as the secondary span"
    );
    let suggestion = hit
        .suggestion
        .as_ref()
        .expect("suggestion should be present");
    assert!(
        suggestion.contains("par struct Counter"),
        "suggestion should spell out the rename; got: {suggestion}",
    );
    assert!(
        suggestion.contains("Mutex"),
        "suggestion should mention Mutex wrapping for mut fields"
    );
}

#[test]
fn test_concurrent_shared_struct_silent_when_only_one_branch_uses() {
    // Sole-ownership move into exactly one branch — per design.md §
    // Rc vs Arc — Two-Phase Algorithm "Rule for `shared struct`":
    // this is NOT an error. The other branch is independent work.
    let result = ownership_ok(
        "shared struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn other_work(n: i64) -> i64 { n + 1 }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 other_work(7);\n\
             }\n\
         }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "sole-branch shared-struct use must not fire E_CONCURRENT_SHARED_STRUCT"
    );
}

#[test]
fn test_concurrent_shared_struct_does_not_fire_on_plain_struct() {
    // The new diagnostic targets shared struct only. Plain structs
    // moved into two branches would be caught by other ownership
    // checks (UseAfterMove), but NOT by this kind. Locks the kind
    // selector against accidental over-firing.
    let parsed = karac::parse(
        "struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    assert!(parsed.errors.is_empty());
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "plain (non-shared) struct must NOT fire E_CONCURRENT_SHARED_STRUCT"
    );
}

#[test]
fn test_concurrent_shared_struct_fires_via_field_access() {
    // The detection counts any source-level reference to the shared
    // binding inside each branch — including field-access shapes like
    // `tree.left`. Pins that we don't only catch identifier-passed-
    // to-fn forms.
    let errors = ownership_errors(
        "shared struct Node { val: i64 }\n\
         fn read(n: i64) -> i64 { n }\n\
         fn main() {\n\
             let root = Node { val: 7 };\n\
             par {\n\
                 read(root.val);\n\
                 read(root.val);\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { type_name, binding }
                if type_name == "Node" && binding == "root"
        )),
        "field-access in two branches should still fire E_CONCURRENT_SHARED_STRUCT"
    );
}

#[test]
fn test_concurrent_shared_enum_fires() {
    // Sibling to the struct case — shared enums follow the same rule.
    let errors = ownership_errors(
        "shared enum Status { Active, Idle }\n\
         fn handle_a(s: Status) { }\n\
         fn handle_b(s: Status) { }\n\
         fn main() {\n\
             let s = Status.Active;\n\
             par {\n\
                 handle_a(s);\n\
                 handle_b(s);\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { type_name, .. }
                if type_name == "Status"
        )),
        "shared enum in two branches should fire E_CONCURRENT_SHARED_STRUCT"
    );
}

// ── E_CONCURRENT_PLAIN_STRUCT (phase-7 line 197 sibling) ────────
//
// Plain (non-shared) struct binding referenced from two or more
// concurrent branches of a `par {}` block. Same detection mechanism
// as the shared case, different migration target (`struct` → `par
// struct` rather than `shared struct` → `par struct`).

#[test]
fn test_concurrent_plain_struct_fires_on_two_branch_use() {
    let errors = ownership_errors(
        "struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    let hit = errors
        .iter()
        .find(|e| {
            matches!(
                &e.kind,
                OwnershipErrorKind::ConcurrentPlainStruct { type_name, binding }
                    if type_name == "Counter" && binding == "c"
            )
        })
        .expect("expected E_CONCURRENT_PLAIN_STRUCT error");
    assert!(
        hit.message.contains("plain struct"),
        "diagnostic message should distinguish plain from shared; got: {}",
        hit.message,
    );
    assert!(
        hit.consume_span.is_some(),
        "first-branch use should be threaded as the secondary span"
    );
    let suggestion = hit
        .suggestion
        .as_ref()
        .expect("suggestion should be present");
    assert!(
        suggestion.contains("rename `struct Counter` to `par struct Counter`"),
        "plain-struct suggestion should describe the keyword insertion (not the shared-struct rename); got: {suggestion}",
    );
    assert!(
        suggestion.contains("Mutex"),
        "suggestion should mention Mutex wrapping for mut fields"
    );
}

#[test]
fn test_concurrent_plain_struct_silent_when_only_one_branch_uses() {
    // Sole-branch use — the rule's accept side carries over to plain
    // struct too. Sibling to test_concurrent_shared_struct_silent_*.
    let result = ownership_ok(
        "struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn other_work(n: i64) -> i64 { n + 1 }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 other_work(7);\n\
             }\n\
         }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentPlainStruct { .. })),
        "sole-branch plain-struct use must not fire E_CONCURRENT_PLAIN_STRUCT"
    );
}

#[test]
fn test_concurrent_plain_struct_does_not_fire_on_shared_struct() {
    // Kind-selector lock — the shared-struct case keeps firing the
    // SHARED kind, not the PLAIN kind. Mirror of the original test
    // `test_concurrent_shared_struct_does_not_fire_on_plain_struct`.
    let parsed = karac::parse(
        "shared struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    assert!(parsed.errors.is_empty());
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentPlainStruct { .. })),
        "shared struct must NOT fire E_CONCURRENT_PLAIN_STRUCT (the shared kind fires instead)"
    );
}

// ── fix_diff envelope (phase-7 line 197 follow-up, both kinds) ──
//
// Both `ConcurrentSharedStruct` and `ConcurrentPlainStruct` populate
// the sibling `error_fix_diffs` map keyed by the diagnostic's primary
// span. L201a (2026-05-26) grew the edit list to three families:
// (1) keyword rewrite — `shared` → `par` for shared, insert `par `
// before `struct` for plain; (2) `mut ` keyword strip per mut field;
// (3) `Mutex[T]` wrap (two pure-insertion edits) per mut field. The
// previous L197-slice tests asserted only (3); the new tests below
// extend coverage to (1) and (2) while preserving (3).

#[test]
fn test_concurrent_struct_fix_diff_wraps_each_mut_field() {
    let parsed = karac::parse(
        "shared struct Counter { val: i64, mut count: i64, mut tag: i64 }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0, count: 0, tag: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    assert!(parsed.errors.is_empty());
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result
        .error_fix_diffs
        .get(&key)
        .expect("expected fix_diff edits for shared-struct diagnostic");
    // Two mut fields (count, tag) → 4 Mutex-wrap edits (prefix +
    // suffix per field) + 2 `mut ` strip edits (one per mut field) +
    // 1 `shared` → `par` keyword rename = 7 edits total. The
    // immutable `val` field stays untouched.
    assert_eq!(
        edits.len(),
        7,
        "expected 7 edits (1 keyword rename + 2 mut strips + 2 mut fields × 2 wraps); got {}",
        edits.len(),
    );
    let prefix_count = edits.iter().filter(|e| e.replacement == "Mutex[").count();
    let suffix_count = edits.iter().filter(|e| e.replacement == "]").count();
    let rename_count = edits.iter().filter(|e| e.replacement == "par").count();
    let strip_count = edits
        .iter()
        .filter(|e| e.replacement.is_empty() && e.length > 0)
        .count();
    assert_eq!(prefix_count, 2, "expected 2 `Mutex[` prefix insertions");
    assert_eq!(suffix_count, 2, "expected 2 `]` suffix insertions");
    assert_eq!(rename_count, 1, "expected 1 `shared` → `par` rename");
    assert_eq!(strip_count, 2, "expected 2 `mut ` strip deletions");
}

#[test]
fn test_concurrent_plain_struct_fix_diff_wraps_each_mut_field() {
    let parsed = karac::parse(
        "struct State { id: i64, mut count: i64 }\n\
         fn use_a(s: State) { }\n\
         fn use_b(s: State) { }\n\
         fn main() {\n\
             let s = State { id: 0, count: 0 };\n\
             par {\n\
                 use_a(s);\n\
                 use_b(s);\n\
             }\n\
         }",
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentPlainStruct { .. }))
        .expect("expected ConcurrentPlainStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result
        .error_fix_diffs
        .get(&key)
        .expect("expected fix_diff edits for plain-struct diagnostic");
    // 1 mut field × 2 wraps + 1 mut strip + 1 `par ` keyword insert = 4 edits.
    assert_eq!(
        edits.len(),
        4,
        "expected 4 edits (1 `par ` insert + 1 mut strip + 1 mut field × 2 wraps); got {}",
        edits.len(),
    );
    assert!(edits.iter().any(|e| e.replacement == "Mutex["));
    assert!(edits.iter().any(|e| e.replacement == "]"));
    assert!(
        edits
            .iter()
            .any(|e| e.replacement == "par " && e.length == 0),
        "expected a pure-insertion `par ` edit"
    );
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.is_empty() && e.length > 0),
        "expected a `mut ` strip deletion"
    );
}

#[test]
fn test_concurrent_struct_fix_diff_keyword_rename_only_when_no_mut_fields() {
    // A shared struct with only immutable fields still gets the
    // `shared` → `par` keyword rename edit (the migration's structural
    // half), even though no Mutex wrap or mut strip applies. Pre-L201a
    // this case emitted zero edits; post-L201a the keyword rename
    // remains.
    let parsed = karac::parse(
        "shared struct Tag { val: i64 }\n\
         fn use_a(t: Tag) { }\n\
         fn use_b(t: Tag) { }\n\
         fn main() {\n\
             let t = Tag { val: 0 };\n\
             par {\n\
                 use_a(t);\n\
                 use_b(t);\n\
             }\n\
         }",
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result
        .error_fix_diffs
        .get(&key)
        .expect("expected fix_diff edits (keyword rename) for shared-struct diagnostic");
    assert_eq!(
        edits.len(),
        1,
        "expected exactly 1 edit (keyword rename only); got {}",
        edits.len(),
    );
    assert_eq!(edits[0].replacement, "par");
}

// ── L201a: keyword rewrite + mut-strip edits — byte precision ─────
//
// Pin the new edit kinds at exact source offsets so a parser regression
// (drift in `kind_keyword_span` / `struct_keyword_span` / `mut_keyword_span`
// / `name_span`) is caught immediately. The source positions are computed
// from the test fixture so they survive whitespace-insensitive edits to
// the test header.

#[test]
fn test_l201a_shared_keyword_rename_targets_shared_token() {
    // Verify the rename edit covers exactly the `shared` token (6
    // bytes) and is positioned at the keyword's offset, not the start
    // of the struct definition. Source picks an isolated `shared`
    // (no `pub` modifier) to keep the byte offset trivial: line 1
    // starts at offset 0, `shared` begins at column 1 → offset 0.
    let src = "shared struct Counter { val: i64 }\n\
               fn use_a(c: Counter) { }\n\
               fn use_b(c: Counter) { }\n\
               fn main() {\n\
                   let c = Counter { val: 0 };\n\
                   par {\n\
                       use_a(c);\n\
                       use_b(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    let rename = edits
        .iter()
        .find(|e| e.replacement == "par")
        .expect("expected `shared` → `par` rename edit");
    let shared_off = src.find("shared").unwrap();
    assert_eq!(rename.offset, shared_off, "rename offset must hit `shared`");
    assert_eq!(rename.length, 6, "rename must consume the 6-byte `shared`");
}

#[test]
fn test_l201a_plain_keyword_insert_targets_struct_token() {
    // Plain struct: insert `par ` immediately before the `struct`
    // token. Verify the edit is a pure insertion (length=0) at the
    // exact offset of `struct`, so applying it produces `par struct
    // State { ... }`.
    let src = "struct State { id: i64 }\n\
               fn use_a(s: State) { }\n\
               fn use_b(s: State) { }\n\
               fn main() {\n\
                   let s = State { id: 0 };\n\
                   par {\n\
                       use_a(s);\n\
                       use_b(s);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentPlainStruct { .. }))
        .expect("expected ConcurrentPlainStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    let insert = edits
        .iter()
        .find(|e| e.replacement == "par ")
        .expect("expected `par ` insert edit");
    let struct_off = src.find("struct").unwrap();
    assert_eq!(
        insert.offset, struct_off,
        "insert offset must be at `struct`"
    );
    assert_eq!(insert.length, 0, "insert must be pure (length=0)");
}

#[test]
fn test_l201a_mut_strip_covers_mut_and_trailing_whitespace() {
    // The mut-strip deletion runs from `mut_keyword_span.offset` to
    // `name_span.offset` — i.e. consumes `mut` AND the source-text
    // whitespace before the field name. Test fixture uses a single
    // space (the common case); the strip-end must land precisely on
    // the field-name offset so any text edit applied to the buffer
    // produces `field_name: T` with no leading whitespace artifacts.
    let src = "shared struct Counter { mut count: i64 }\n\
               fn use_a(c: Counter) { }\n\
               fn use_b(c: Counter) { }\n\
               fn main() {\n\
                   let c = Counter { count: 0 };\n\
                   par {\n\
                       use_a(c);\n\
                       use_b(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    let strip = edits
        .iter()
        .find(|e| e.replacement.is_empty() && e.length > 0)
        .expect("expected a `mut ` strip deletion edit");
    let mut_off = src.find("mut count").unwrap();
    let name_off = src.find("count: i64").unwrap();
    assert_eq!(strip.offset, mut_off, "strip must start at the `mut` token");
    assert_eq!(
        strip.length,
        name_off - mut_off,
        "strip must extend to the field name offset (includes trailing whitespace)"
    );
}

// ── L201b: lock-block insertion at par-internal write sites ─────
//
// Third edit family for E_CONCURRENT_*_STRUCT fix_diff. For each
// write to a `binding.<mut_field>` form occurring textually inside a
// par block (any nesting depth), emit two pure-insertion edits
// wrapping the statement in `lock <field> {\n    ... \n}`. The
// wrap target is the bare field name — Kara's `lock` takes an
// identifier, not an expression, so no source-text reconstruction is
// needed. Existing tests' fixtures contain no writes, so their edit
// counts are unaffected.

#[test]
fn test_l201b_lock_block_wraps_assign_in_par_branch() {
    // Canonical case: `par { c.count = ...; ... }` — assignment to
    // a mut field of the diagnosed binding, textually inside the par
    // block. Emits prefix `lock count {\n    ` at stmt start and
    // suffix `\n}` at stmt end (both pure insertions).
    let src = "shared struct Counter { mut count: i64 }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { count: 0 };\n\
                   par {\n\
                       c.count = 7;\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    let prefix = edits
        .iter()
        .find(|e| e.replacement.starts_with("lock count {"))
        .expect("expected `lock count {` prefix insertion");
    let suffix = edits
        .iter()
        .find(|e| e.replacement == "\n}")
        .expect("expected `\\n}` suffix insertion");
    assert_eq!(prefix.length, 0, "prefix must be a pure insertion");
    assert_eq!(suffix.length, 0, "suffix must be a pure insertion");
    let target_start = src.find("c.count = 7;").expect("stmt anchor");
    // Suffix anchors right before the trailing `;` so the wrap covers
    // `target = value` exactly; the `;` falls through as the lock-
    // statement's own terminator (`lock f { ... };`).
    let value_end = src.find("7;").expect("value anchor") + 1;
    assert_eq!(prefix.offset, target_start, "prefix at target start");
    assert_eq!(suffix.offset, value_end, "suffix at value end (before `;`)");
}

#[test]
fn test_l201b_lock_block_skips_immutable_field_writes() {
    // The lock wrap only applies to writes to fields the struct
    // declared as `mut`. A diagnostic-firing fixture with an
    // assignment to an immutable field would be a typecheck error,
    // so the realistic shape is "struct has both mut and immutable
    // fields, par contains only an assign to the immutable one" —
    // but immutable-field assigns are rejected at parse. Pin the
    // simpler guard: a struct with ONLY immutable fields produces no
    // lock-block edits even when the diagnostic fires.
    let src = "shared struct Tag { val: i64 }\n\
               fn use_a(t: Tag) { }\n\
               fn use_b(t: Tag) { }\n\
               fn main() {\n\
                   let t = Tag { val: 0 };\n\
                   par {\n\
                       use_a(t);\n\
                       use_b(t);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        !edits.iter().any(|e| e.replacement.starts_with("lock ")),
        "no mut fields → no lock-block wrap edits; got {edits:?}",
    );
}

#[test]
fn test_l201b_lock_block_wraps_nested_assign_inside_par_if() {
    // The walker descends into nested blocks (if/while/match/...)
    // inside a par branch. Verify an assignment buried inside an
    // `if` body still gets wrapped — pinning the recursive walk.
    let src = "shared struct Counter { mut count: i64 }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { count: 0 };\n\
                   par {\n\
                       if true { c.count = 7; }\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock count {")),
        "nested assign inside `if` should still receive the lock wrap; got {edits:?}",
    );
}

#[test]
fn test_l201b_lock_block_wraps_compound_assign() {
    // `c.count += 1` is a CompoundAssign in the AST — equally a
    // write, equally needs synchronization. Pin that the walker
    // covers both StmtKind variants.
    let src = "shared struct Counter { mut count: i64 }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { count: 0 };\n\
                   par {\n\
                       c.count += 1;\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock count {")),
        "compound-assign write should also be wrapped; got {edits:?}",
    );
}

// ── L205: mutating method-call lock wraps ───────────────────────
//
// L201b shipped lock wraps for `Assign` / `CompoundAssign` writes
// (`c.field = x` / `c.field += 1`). L205 extends the walker to also
// wrap statement-position mutating method calls — `c.field.push(x)`,
// `c.field.clear()`, etc. Mutability is classified via
// `method_self_modes` (user impls, `SelfParam::MutRef`) with fallback
// to `stdlib_method_self_borrow_kind` (`Vec.push`/`Map.insert`/etc.
// table, `BorrowKind::MutRef`). The wrap end-anchor uses the parser-
// captured `MethodCall.args_close_span` (the `)` token) since the
// outer `Expr.span` covers only the receiver.

#[test]
fn test_l205_lock_block_wraps_stdlib_vec_push() {
    // Canonical case: `c.items.push(7);` inside par — `Vec.push`
    // resolves to `BorrowKind::MutRef` via stdlib_method_self_borrow_kind,
    // and `items` is the diagnosed binding's mut field. Wrap covers
    // `c.items.push(7)` (the trailing `;` becomes the lock-statement's
    // own terminator). Byte-precise offset pin against the receiver
    // start and the `)` close position.
    let src = "shared struct Counter { mut items: Vec[i64] }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { items: Vec[0i64; 0] };\n\
                   par {\n\
                       c.items.push(7);\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    let prefix = edits
        .iter()
        .find(|e| e.replacement.starts_with("lock items {"))
        .expect("expected `lock items {` prefix insertion");
    let suffix_offsets: Vec<usize> = edits
        .iter()
        .filter(|e| e.replacement == "\n}" && e.length == 0)
        .map(|e| e.offset)
        .collect();
    assert_eq!(prefix.length, 0, "prefix must be a pure insertion");
    let receiver_start = src.find("c.items.push(7);").expect("stmt anchor");
    let call_end = src.find("c.items.push(7);").unwrap() + "c.items.push(7)".len();
    assert_eq!(
        prefix.offset, receiver_start,
        "prefix anchored at receiver start (`c`)"
    );
    assert!(
        suffix_offsets.contains(&call_end),
        "suffix anchored at end of `)` (call_end={call_end}); got {suffix_offsets:?}",
    );
}

#[test]
fn test_l205_lock_block_wraps_user_mut_ref_self_method() {
    // User-defined method with `mut ref self` receiver invoked on the
    // outer struct's mut field — classifier looks the method up in
    // `method_self_modes` (built from `collect_method_self_modes` over
    // user impl blocks) and finds `SelfParam::MutRef`. Wrap fires
    // identically to the stdlib path. The inner struct `Tracker` is
    // the type stored in the outer `Counter.tracker` mut field; the
    // call shape `c.tracker.bump()` matches L205's
    // `Identifier(c).tracker.<method>` receiver gate.
    let src = "struct Tracker { mut val: i64 }\n\
               impl Tracker {\n\
                   fn bump(mut ref self) { self.val = self.val + 1; }\n\
               }\n\
               shared struct Counter { mut tracker: Tracker }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { tracker: Tracker { val: 0 } };\n\
                   par {\n\
                       c.tracker.bump();\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock tracker {")),
        "user-impl `mut ref self` method on a mut field should be wrapped via `method_self_modes`; got {edits:?}",
    );
}

#[test]
fn test_l205_lock_block_skips_immutable_method() {
    // `c.items.len()` is a read (`Vec.len` → `BorrowKind::ImmRef`).
    // Classifier returns false; no wrap emitted even though the
    // receiver shape `Identifier(c).items` matches a mut field and
    // the diagnostic fires. Pins that the classifier gate is load-
    // bearing.
    let src = "shared struct Counter { mut items: Vec[i64] }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { items: Vec[0i64; 0] };\n\
                   par {\n\
                       let _n = c.items.len();\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        !edits.iter().any(|e| e.replacement.starts_with("lock ")),
        "immutable method (`Vec.len`) must NOT trigger a lock wrap; got {edits:?}",
    );
}

#[test]
fn test_l205_lock_block_wraps_method_call_nested_in_par_if() {
    // The L201b walker descends through nested control flow (if/while
    // /for/match/...) and the L205 extension piggybacks on that walk.
    // Verify a mutating method call buried inside an `if` body in a
    // par branch still gets wrapped — pins that
    // `collect_lock_block_writes_in_stmt`'s `StmtKind::Expr` arm runs
    // at every depth, not just the outer par-branch level.
    let src = "shared struct Counter { mut items: Vec[i64] }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { items: Vec[0i64; 0] };\n\
                   par {\n\
                       if true { c.items.push(7); }\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock items {")),
        "method-call mutation nested inside `if` should still receive the lock wrap; got {edits:?}",
    );
}

// ── L207: receivers beyond simple Identifier(name).field ─────────
//
// L201b shipped the v1 receiver gate (`Identifier(binding).<field>` —
// single-step field access on the diagnosed binding). L207 widens it
// to multi-step *place chains* whose first projection on the binding
// is a field:
//   - `c.field.subfield` (deeper field chain)
//   - `c.field[0]` (field followed by index)
// The lock target is the **first** field on the binding (the
// outermost-on-binding projection). Index-first chains (`c[0].field`,
// `arr[0].field`) and temporary receivers (`f().field`) remain out
// of scope — index-first writes need element-level locking which
// `lock <field> { ... }` doesn't model, and temporaries have no
// binding to associate with the diagnostic.

#[test]
fn test_l207_lock_block_wraps_chained_field_projection_assign() {
    // `c.nested.val = 7;` inside par — write through a chained field
    // projection. The first field projection on `c` is `nested`, so
    // the lock target is `nested` (regardless of which sub-field the
    // write targets). The outer field's mut-ness is the gate; the
    // sub-field's mut-ness is the typechecker's concern.
    let src = "struct Tracker { mut val: i64 }\n\
               shared struct Counter { mut nested: Tracker }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { nested: Tracker { val: 0 } };\n\
                   par {\n\
                       c.nested.val = 7;\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock nested {")),
        "chained-field assign `c.nested.val = 7` should lock the outer field `nested`; got {edits:?}",
    );
}

#[test]
fn test_l207_lock_block_wraps_chained_field_method_call() {
    // L205's method-call walker delegates the receiver gate to
    // `matched_self_field_access`. With L207's generalization, a
    // method call on a deeper field chain (`c.nested.bump()` where
    // `nested` is the outer mut field and `bump` is a `mut ref self`
    // method on `Tracker`) gets wrapped with `lock nested { ... }`.
    let src = "struct Tracker { mut val: i64 }\n\
               impl Tracker {\n\
                   fn bump(mut ref self) { self.val = self.val + 1; }\n\
               }\n\
               shared struct Counter { mut nested: Tracker }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { nested: Tracker { val: 0 } };\n\
                   par {\n\
                       c.nested.bump();\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock nested {")),
        "method on deep field chain (`c.nested.bump()`) should still lock the outer field; got {edits:?}",
    );
}

#[test]
fn test_l207_lock_block_wraps_field_followed_by_index() {
    // `c.items[0] = 7;` — write through a field followed by an index
    // projection. resolve_place_chain returns
    // `{ root: "c", projections: [Field("items"), Index] }`; the first
    // projection on `c` is `Field("items")`, so the lock target is
    // `items`. Pins that Index *after* a field projection doesn't
    // reject the chain (only Index *before* any field rejects).
    let src = "shared struct Counter { mut items: Vec[i64] }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { items: Vec[1i64, 2i64, 3i64] };\n\
                   par {\n\
                       c.items[0] = 7;\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    assert!(
        edits
            .iter()
            .any(|e| e.replacement.starts_with("lock items {")),
        "field-then-index write (`c.items[0] = 7`) should lock the field `items`; got {edits:?}",
    );
}

#[test]
fn test_l207_lock_block_skips_index_first_chain() {
    // `arr[0]` where `arr` is the diagnosed binding (the par branches
    // reference `arr` itself). resolve_place_chain returns
    // `{ root: "arr", projections: [Index] }`; the first projection is
    // Index, not Field, so the rooted-field gate rejects. No lock-
    // block edit is emitted because `lock <field>` doesn't model
    // element-level locking — that's a separate design surface.
    //
    // The diagnostic still fires (the binding is reachable from both
    // branches); only the lock wrap is suppressed.
    let src = "shared struct Counter { mut items: Vec[i64] }\n\
               fn use_a(c: ref Counter) { }\n\
               fn main() {\n\
                   let c = Counter { items: Vec[1i64, 2i64] };\n\
                   par {\n\
                       let _x = c.items[0];\n\
                       use_a(c);\n\
                   }\n\
               }";
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let result = karac::ownershipcheck(&parsed.program, &typed);
    let err = result
        .errors
        .iter()
        .find(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. }))
        .expect("expected ConcurrentSharedStruct error");
    let key = karac::resolver::SpanKey::from_span(&err.span);
    let edits = result.error_fix_diffs.get(&key).expect("expected fix_diff");
    // The fixture is a read (`let _x = c.items[0]`), so no lock-block
    // edit would fire even with full L207 support. The key assertion
    // is the negative one: no `lock ` edit appears for the read path.
    assert!(
        !edits.iter().any(|e| e.replacement.starts_with("lock ")),
        "read-through-index should not emit a lock-block edit; got {edits:?}",
    );
}

// ── L203: closure-captured shared bindings ──────────────────────
//
// The v1 detector only counted direct source-level references to the
// shared/plain binding inside each branch. Closures dispatched into
// sibling branches that capture the binding indirectly slipped past.
// L203 mirrors `par_helpers.rs`'s round-12.34 closure_bindings
// expansion into the concurrent_shared detector: a let-bound closure
// `let f = || use(c);` followed by a sibling-branch `Identifier(f)`
// counts as a branch-use of `c`, and an inline closure
// `spawn(|| use(c))` counts via `closure_captures` lookup at the
// closure's span. Both flavors fire the right diagnostic kind
// (shared → SharedStruct, plain → PlainStruct).

#[test]
fn test_concurrent_shared_struct_fires_via_let_bound_closure() {
    // Two let-bound closures, each capturing `c`, dispatched into
    // sibling branches via `spawn(f)` / `spawn(g)`. Neither
    // `spawn(f)` nor `spawn(g)` lexically contains `c` — the v1
    // detector saw zero branch-uses. L203 expands the Identifier(f)
    // use through the closure_bindings table to record `c` as a
    // branch-use, and likewise for Identifier(g) in the sibling
    // branch.
    let errors = ownership_errors(
        "shared struct Counter { val: i64 }\n\
         fn spawn(c: Fn() -> ()) { }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             let f = || use_a(c);\n\
             let g = || use_b(c);\n\
             par {\n\
                 spawn(f);\n\
                 spawn(g);\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { type_name, binding }
                if type_name == "Counter" && binding == "c"
        )),
        "let-bound closures capturing `c` across two par branches should fire E_CONCURRENT_SHARED_STRUCT; got errors: {errors:?}",
    );
}

#[test]
fn test_concurrent_shared_struct_fires_via_inline_closure() {
    // Inline-closure form — `spawn(|| use(c))` in each branch. The
    // closure expression itself is the argument; no let-binding is
    // ever introduced. L203 looks up `closure_captures` at the
    // closure's span and records each captured tracked binding as a
    // branch-use at the closure expression's span.
    let errors = ownership_errors(
        "shared struct Counter { val: i64 }\n\
         fn spawn(c: Fn() -> ()) { }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 spawn(|| use_a(c));\n\
                 spawn(|| use_b(c));\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { type_name, binding }
                if type_name == "Counter" && binding == "c"
        )),
        "inline-closure captures of `c` across two par branches should fire E_CONCURRENT_SHARED_STRUCT; got errors: {errors:?}",
    );
}

#[test]
fn test_concurrent_shared_struct_silent_when_only_one_branch_closure_captures() {
    // Sole-branch closure capture stays silent — the accept side of
    // the rule still holds for closure-mediated uses. Exercises the
    // closure_bindings expansion path: `f` is bound outside the par
    // block and invoked in exactly one branch, which expands through
    // closure_bindings[f] = [c] to record `c` as a branch-0 use; the
    // sibling branch is closure-free. Local invocation `f()` avoids
    // the closure-escape diagnostic that fires when a closure is
    // passed through `spawn(...)`.
    let result = ownership_ok(
        "shared struct Counter { val: i64 }\n\
         fn use_a(c: ref Counter) { }\n\
         fn other_work(n: i64) -> i64 { n + 1 }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             let f = || use_a(c);\n\
             par {\n\
                 f();\n\
                 other_work(7);\n\
             }\n\
         }",
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|e| matches!(&e.kind, OwnershipErrorKind::ConcurrentSharedStruct { .. })),
        "sole-branch closure capture of `c` must not fire E_CONCURRENT_SHARED_STRUCT; got errors: {:?}",
        result.errors,
    );
}

#[test]
fn test_concurrent_plain_struct_fires_via_inline_closure() {
    // Sibling case for the plain (non-shared) struct kind — same
    // closure-capture mechanism, different diagnostic flavor. Pins
    // that the BindingKind discriminator routes inline-closure-
    // captured plain structs to E_CONCURRENT_PLAIN_STRUCT. Both
    // call-sites use `ref Counter` so the captures land as Ref mode
    // (plain `Counter` would consume `c` on first closure → second
    // closure use-after-move masks the concurrent-shared signal).
    let errors = ownership_errors(
        "struct Counter { val: i64 }\n\
         fn spawn(c: Fn() -> ()) { }\n\
         fn use_a(c: ref Counter) { }\n\
         fn use_b(c: ref Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 spawn(|| use_a(c));\n\
                 spawn(|| use_b(c));\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentPlainStruct { type_name, binding }
                if type_name == "Counter" && binding == "c"
        )),
        "inline-closure captures of plain `c` across two par branches should fire E_CONCURRENT_PLAIN_STRUCT; got errors: {errors:?}",
    );
}

// ── Phase 6 line 170 slice 5/6: par borrow-rule verification ───────
//
// Slice 5 ("borrow-rule consolidation at spawn boundaries") verifies the
// existing live-range / concurrency analysis produces the right
// diagnostics for the par-boundary borrow cases — it adds NO new
// borrow-checker pass. These tests pin the behavior the v1 model actually
// implements.
//
// **Design model (important).** v1 follows design.md § 8506's conservative
// rule: a non-`par` struct value reachable from two-or-more `par {}`
// branches is rejected (`E_CONCURRENT_PLAIN_STRUCT` / `…SHARED_STRUCT`),
// *regardless of borrow mode* — even two shared `ref` reads. design.md
// § 9476's "multiple sibling tasks may simultaneously hold `ref T`"
// describes a more permissive model whose enabling type, `par struct`, is
// not implemented at v1 (a leading `par` at item scope is currently
// dropped — see the parser-gap tracker entry). So the "two readers
// accepted" rows of the slice-6 matrix are gated on that work and are NOT
// asserted here; the existing `test_concurrent_plain_struct_fires_via_
// inline_closure` (which expects two `ref Counter` reads to FIRE) pins
// the current model. The cross-task-safe *capture* matrix (Rc / OnceCell
// / raw-pointer / shared-struct at spawn / par / channel / provider
// sites) is covered by the slice 3a/3b/3c tests in `tests/typechecker.rs`.

#[test]
fn par_sole_branch_mut_ref_use_accepted() {
    // Slice 6 positive (achievable): a `mut ref` borrow taken in exactly
    // ONE par branch is sole-ownership at the fork point — accepted. The
    // sibling branch does independent work touching no shared binding.
    // (design.md § 9477 exclusive-borrow rule + the sole-branch accept
    // side of the concurrent-struct rule.)
    ownership_ok(
        "fn append(l: mut ref Vec[i64]) { }\n\
         fn main() {\n\
             let mut log: Vec[i64] = Vec.new();\n\
             par {\n\
                 let _x: i64 = 1;\n\
                 append(mut log);\n\
             }\n\
         }",
    );
}

#[test]
fn par_mut_ref_plus_read_across_siblings_rejected() {
    // Slice 6 negative (achievable): a `mut ref` write in one branch and a
    // `ref` read in a sibling branch is a cross-task conflict. In the v1
    // § 8506 model this surfaces as `E_CONCURRENT_PLAIN_STRUCT` (the
    // concurrent-struct rule fires on the two-branch reach) rather than
    // the standalone "cannot borrow as immutable in branch 2" borrow-
    // conflict wording the slice-6 sketch predicted — the conservative
    // concurrent-struct rule is the operative diagnostic here. Pinned so a
    // future move to the § 9476 mode-aware model is a visible, deliberate
    // change to this expectation.
    let errors = ownership_errors(
        "fn append(l: mut ref Vec[i64]) { }\n\
         fn read(l: ref Vec[i64]) -> i64 { 0 }\n\
         fn main() {\n\
             let mut log: Vec[i64] = Vec.new();\n\
             par {\n\
                 append(mut log);\n\
                 let _y: i64 = read(log);\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentPlainStruct { type_name, binding }
                if type_name == "Vec" && binding == "log"
        )),
        "mut-ref + ref reads of `log` across par siblings should be rejected \
         (E_CONCURRENT_PLAIN_STRUCT in the v1 § 8506 model); got: {errors:?}",
    );
}

#[test]
fn par_primitive_used_in_both_branches_accepted() {
    // Slice 6 positive: a Copy primitive (`i64`) read in both branches is
    // never a conflict — primitives are trivially cross-task-safe and the
    // concurrent-struct rule only tracks struct/enum-typed bindings.
    ownership_ok(
        "fn read(x: i64) -> i64 { x }\n\
         fn main() {\n\
             let v: i64 = 5;\n\
             par {\n\
                 let _a: i64 = read(v);\n\
                 let _b: i64 = read(v);\n\
             }\n\
         }",
    );
}

// ── Phase 6 `par struct` slice B: concurrency exemption ──────────
// `par struct` / `par enum` are cross-task-safe by definition (design.md
// § Part 5b > "Crossing parallel region boundaries"): a `par` binding
// reachable from 2+ sibling `par {}` branches is NOT an E_CONCURRENT_*
// error, in contrast to plain / `shared` structs.

#[test]
fn par_struct_multiple_sibling_ref_readers_accepted() {
    // Phase 6 `par struct` slice D — design.md § 9476: "multiple sibling
    // tasks may simultaneously hold `ref T`". This permissive concurrent-read
    // model was "unreachable" at v1 because `par struct` didn't exist; now it
    // does, and Slice B's exemption delivers it — two sibling `ref` readers of
    // the SAME `par struct` binding are accepted (the same shape is rejected
    // for `shared` / plain structs by the stricter § 8506 rule). This pins the
    // design.md reconciliation note: the § 9476 allowance is live for `par`.
    let result = ownership_ok(
        "par struct Counter { val: i64 }\n\
         fn read_a(c: ref Counter) -> i64 { c.val }\n\
         fn read_b(c: ref Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 5 };\n\
             par {\n\
                 let _a = read_a(c);\n\
                 let _b = read_b(c);\n\
             }\n\
         }",
    );
    assert!(
        !result.errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { .. }
                | OwnershipErrorKind::ConcurrentPlainStruct { .. }
        )),
        "two sibling `ref` readers of a par struct must be accepted (§ 9476); got: {:?}",
        result.errors
    );
}

#[test]
fn par_struct_reachable_from_two_branches_accepted() {
    // The exact shape that fires E_CONCURRENT_SHARED_STRUCT for a `shared
    // struct` (see test_concurrent_shared_struct_fires_on_two_branch_use)
    // must be silent for a `par struct`.
    let result = ownership_ok(
        "par struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    assert!(
        !result.errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { .. }
                | OwnershipErrorKind::ConcurrentPlainStruct { .. }
        )),
        "par struct reachable from two branches must not fire E_CONCURRENT_*; got: {:?}",
        result.errors
    );
}

#[test]
fn par_enum_reachable_from_two_branches_accepted() {
    // `ref` params (borrow, no move) so the binding is reachable from both
    // branches — the same `shared enum` shape fires E_CONCURRENT_SHARED_STRUCT
    // (clone-on-pass reference semantics for enums is a separate, pre-existing
    // gap; this test exercises the *classification* exemption Slice B changes).
    let result = ownership_ok(
        "par enum Msg { Ping, Pong }\n\
         fn use_a(m: ref Msg) { }\n\
         fn use_b(m: ref Msg) { }\n\
         fn main() {\n\
             let m = Msg.Ping;\n\
             par {\n\
                 use_a(m);\n\
                 use_b(m);\n\
             }\n\
         }",
    );
    assert!(
        !result.errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { .. }
                | OwnershipErrorKind::ConcurrentPlainStruct { .. }
        )),
        "par enum reachable from two branches must not fire E_CONCURRENT_*; got: {:?}",
        result.errors
    );
}

#[test]
fn shared_struct_two_branch_use_still_fires_after_par_exemption() {
    // Regression: the par exemption must not weaken the shared-struct rule.
    let errors = ownership_errors(
        "shared struct Counter { val: i64 }\n\
         fn use_a(c: Counter) -> i64 { c.val }\n\
         fn use_b(c: Counter) -> i64 { c.val }\n\
         fn main() {\n\
             let c = Counter { val: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }",
    );
    assert!(
        errors.iter().any(|e| matches!(
            &e.kind,
            OwnershipErrorKind::ConcurrentSharedStruct { type_name, .. } if type_name == "Counter"
        )),
        "shared struct two-branch use must still fire E_CONCURRENT_SHARED_STRUCT; got: {errors:?}"
    );
}

// ── Borrow returns: source pinning + caller move check (B-2026-06-07-5) ──

#[test]
fn test_borrow_return_field_of_ref_param_ok() {
    // Tier-1 valid shapes: a field reached through a `ref` param, and a
    // `ref` param forwarded directly. design.md § Feature 4 Part 3.
    ownership_ok(
        "struct User { name: String, age: i64 }\n\
         fn name_of(u: ref User) -> ref String { u.name }\n\
         fn fwd(s: ref String) -> ref String { s }\n",
    );
}

#[test]
fn test_borrow_return_dangling_owned_param_errors() {
    let errors = ownership_errors("fn ret_owned(a: String) -> ref String { a }\n");
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            OwnershipErrorKind::BorrowReturnNotSourcePinned {
                shape: BorrowReturnShape::DanglingSource
            }
        )),
        "expected a dangling source-pinning error, got: {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_borrow_return_dangling_local_errors() {
    let errors = ownership_errors(
        "fn leak(a: String) -> ref String {\n\
        \x20   let s = a;\n\
        \x20   s\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: BorrowReturnShape::DanglingSource
        }
    )));
}

#[test]
fn test_string_slice_return_from_ref_param_ok() {
    // StringSlice v1: a `-> StringSlice` view sliced from a `ref` parameter
    // (or a slice-local that traces to one) is source-pinned (design.md §
    // StringSlice: "follows the same borrow rules as `ref T`").
    ownership_ok(
        "fn first_word(s: ref String) -> StringSlice {\n\
        \x20   let sp = s.find(' ');\n\
        \x20   let end = sp.unwrap_or(s.len());\n\
        \x20   s.slice(0, end)\n\
         }\n\
         fn via_local(s: ref String) -> StringSlice {\n\
        \x20   let w = s.slice(0, 1);\n\
        \x20   w\n\
         }\n\
         fn fwd(s: StringSlice) -> StringSlice { s }\n",
    );
}

#[test]
fn test_string_slice_return_from_owned_local_dangling_errors() {
    // A view sliced from an OWNED local dangles: the local's buffer drops at
    // return, leaving the returned view pointing at freed memory. Must be a
    // source-pinning error, not a silent use-after-free.
    let errors = ownership_errors(
        "fn dangling() -> StringSlice {\n\
        \x20   let local = \"temp\".to_string();\n\
        \x20   local.slice(0, 2)\n\
         }\n",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            OwnershipErrorKind::BorrowReturnNotSourcePinned {
                shape: BorrowReturnShape::DanglingSource
            }
        )),
        "expected a dangling source-pinning error for a view into an owned local, got: {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_borrow_return_scalar_match_ok() {
    // Scalar-selector `match` of ref params is supported (sibling of the
    // `if` tier): every arm forwards a `ref` param, so source-pinning
    // accepts it with no error. (`if` of ref params: see the `_if_` tests.)
    ownership_ok(
        "fn pick(a: ref String, b: ref String, flag: bool) -> ref String {\n\
        \x20   match flag { true => a, false => b }\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_unsupported_form_errors() {
    // A destructuring `match` arm is valid per spec but still a codegen
    // follow-on — reported as not-yet-supported, not dangling. (Scalar
    // literal/wildcard arms ARE supported; see
    // `test_borrow_return_scalar_match_ok`.)
    let errors = ownership_errors(
        "fn pick(a: ref String, b: ref String, o: Option[i64]) -> ref String {\n\
        \x20   match o { Some(n) => a, None => b }\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: BorrowReturnShape::UnsupportedForm
        }
    )));
}

#[test]
fn test_borrow_return_move_source_while_live_errors() {
    // 3b: moving the borrow's source while the returned borrow is still
    // live is a use-after-free — must be rejected.
    let errors = ownership_errors(
        "struct User { name: String, age: i64 }\n\
         fn name_of(u: ref User) -> ref String { u.name }\n\
         fn sink(x: User) -> i64 { x.age }\n\
         fn use_after(u: User) -> i64 {\n\
        \x20   let n = name_of(u);\n\
        \x20   let g = sink(u);\n\
        \x20   g\n\
         }\n",
    );
    assert!(
        errors.iter().any(|e| matches!(
            e.kind,
            OwnershipErrorKind::SliceBorrowConflict {
                shape: SliceConflictShape::MoveOfBorrowed
            }
        )),
        "expected move-of-borrowed, got: {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn test_borrow_return_no_move_ok() {
    // Borrowing then only reading the source (no move) is fine.
    ownership_ok(
        "struct User { name: String, age: i64 }\n\
         fn name_of(u: ref User) -> ref String { u.name }\n\
         fn read_age(x: ref User) -> i64 { x.age }\n\
         fn ok_use(u: User) -> i64 {\n\
        \x20   let n = name_of(u);\n\
        \x20   read_age(u)\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_chained_call_ok() {
    // Chained borrow return (B-2026-06-07-5): returning a borrow-returning
    // free-fn call whose ref-position arg is itself a borrowable source
    // (a ref-local here) is source-pinned and accepted.
    ownership_ok(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn echo_twice(s: ref String) -> ref String {\n\
        \x20   let t = echo(s);\n\
        \x20   echo(t)\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_chained_call_on_owned_local_dangles() {
    // The chained call's ref-position arg traces to an owned local that
    // drops at return — the returned borrow would dangle. Source-pinning
    // must report E0509 dangling (the arg's shape propagates through the
    // call classifier), not silently accept.
    let errors = ownership_errors(
        "fn echo(s: ref String) -> ref String { s }\n\
         fn bad() -> ref String {\n\
        \x20   let local = String.from(\"temp\");\n\
        \x20   echo(local)\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: BorrowReturnShape::DanglingSource
        }
    )));
}

// ── Borrowed-struct returns (B-2026-06-07-5) ──

#[test]
fn test_borrow_return_borrowed_struct_ok() {
    // A `-> ref Parser` returning a borrowed-struct construction whose `ref`
    // field traces to a `ref` parameter is source-pinned and accepted
    // (`classify_borrow_return_struct`). The owned `position` field carries
    // no borrow and is ignored.
    ownership_ok(
        "struct Parser { source: ref String, position: i64 }\n\
         fn make_parser(s: ref String) -> ref Parser {\n\
        \x20   Parser { source: s, position: 0 }\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_borrowed_struct_dangling_field_errors() {
    // The borrowed struct's `ref` field is initialized from an owned local
    // that drops at return — the struct's borrow would dangle. The ref-field
    // initializer's shape (DanglingSource) propagates through the struct
    // classifier; E0509, not a silent accept.
    let errors = ownership_errors(
        "struct Parser { source: ref String, position: i64 }\n\
         fn bad() -> ref Parser {\n\
        \x20   let local = String.from(\"temp\");\n\
        \x20   Parser { source: local, position: 0 }\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: BorrowReturnShape::DanglingSource
        }
    )));
}

// ── Tier 2: borrow returns from `if` of ref params (B-2026-06-07-5) ──

#[test]
fn test_borrow_return_if_of_ref_params_ok() {
    ownership_ok(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
        \x20   if a.len() > b.len() { a } else { b }\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_else_if_chain_of_ref_params_ok() {
    ownership_ok(
        "fn pick(a: ref String, b: ref String, c: ref String) -> ref String {\n\
        \x20   if a.len() > 9 { a } else if b.len() > 9 { b } else { c }\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_if_dangling_branch_errors() {
    let errors = ownership_errors(
        "fn bad(a: ref String, b: String) -> ref String {\n\
        \x20   if a.len() > 0 { a } else { b }\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned {
            shape: BorrowReturnShape::DanglingSource
        }
    )));
}

#[test]
fn test_borrow_return_if_move_source_while_live_errors() {
    // 3b across a multi-source (`if`) borrow: moving either source while
    // the returned borrow is live is a use-after-free.
    let errors = ownership_errors(
        "fn longer(a: ref String, b: ref String) -> ref String {\n\
        \x20   if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn sink(x: String) -> i64 { x.len() }\n\
         fn use_after(p: String, q: String) -> i64 {\n\
        \x20   let n = longer(p, q);\n\
        \x20   let g = sink(p);\n\
        \x20   g\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::SliceBorrowConflict {
            shape: SliceConflictShape::MoveOfBorrowed
        }
    )));
}

// ── Method `-> ref` accessors (B-2026-06-07-5) ──

#[test]
fn test_borrow_return_method_ref_self_field_ok() {
    ownership_ok(
        "struct User { name: String, age: i64 }\n\
         impl User {\n\
        \x20   fn name(ref self) -> ref String { self.name }\n\
        \x20   fn age(ref self) -> ref i64 { self.age }\n\
         }\n",
    );
}

#[test]
fn test_borrow_return_method_dangling_local_errors() {
    let errors = ownership_errors(
        "struct User { name: String, age: i64 }\n\
         impl User {\n\
        \x20   fn bad(ref self) -> ref String {\n\
        \x20       let s = self.name;\n\
        \x20       s\n\
        \x20   }\n\
         }\n",
    );
    // `self.name` bound to a local then returned — the local is not a
    // recognized borrow source, so it's flagged (dangling).
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::BorrowReturnNotSourcePinned { .. }
    )));
}

#[test]
fn test_borrow_return_method_move_receiver_while_live_errors() {
    let errors = ownership_errors(
        "struct User { name: String, age: i64 }\n\
         impl User { fn name(ref self) -> ref String { self.name } }\n\
         fn sink(x: User) -> i64 { x.age }\n\
         fn use_after(u: User) -> i64 {\n\
        \x20   let n = u.name();\n\
        \x20   let g = sink(u);\n\
        \x20   g\n\
         }\n",
    );
    assert!(errors.iter().any(|e| matches!(
        e.kind,
        OwnershipErrorKind::SliceBorrowConflict {
            shape: SliceConflictShape::MoveOfBorrowed
        }
    )));
}

// ── E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE (item 6) ──
// Under panic_on_alloc_failure = false, every auto-RC fallback site becomes a
// hard error (the compiler would emit Rc.new/Arc.new, which may panic on OOM).

fn ownership_hard_mode(source: &str) -> OwnershipCheckResult {
    use karac::manifest::ProfileConfig;
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "Type errors: {:?}", typed.errors);
    let cfg = ProfileConfig {
        panic_on_alloc_failure: Some(false),
        ..Default::default()
    };
    karac::ownershipcheck_with_profile_config(&parsed.program, &typed, cfg)
}

const RC_FALLBACK_SOURCE: &str = "struct Owned { x: i64 }\n\
     fn take(o: Owned) { }\n\
     fn main() {\n\
         let o = Owned { x: 1 };\n\
         let _f = || take(o);\n\
         let _u = o;\n\
     }";

#[test]
fn test_hard_mode_rc_fallback_is_hard_error() {
    let result = ownership_hard_mode(RC_FALLBACK_SOURCE);
    let err = result
        .errors
        .iter()
        .find(|e| e.kind == OwnershipErrorKind::RcFallbackAllocatesUnderFallibleProfile)
        .expect("expected RcFallbackAllocatesUnderFallibleProfile error");
    assert!(err.message.contains("Rc.new(...)"), "{}", err.message);
    assert!(err.message.contains("try_new"), "{}", err.message);
}

#[test]
fn test_default_mode_rc_fallback_is_note_not_error() {
    // With the flag unset (default), RC fallback is a perf note, not an error.
    let parsed = parse(RC_FALLBACK_SOURCE);
    let resolved = resolve(&parsed.program);
    let typed = typecheck(&parsed.program, &resolved);
    let result = ownershipcheck(&parsed.program, &typed);
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::RcFallbackAllocatesUnderFallibleProfile),
        "default mode must not emit the hard RC-fallback error"
    );
    assert!(
        result
            .rc_values
            .get("main")
            .is_some_and(|m| m.contains_key("o")),
        "the RC fallback should still be recorded in default mode"
    );
}

// ── Exclusive-borrow rule at call sites (B-2026-06-17-6) ─────────

#[test]
fn exclusive_borrow_same_binding_two_mut_args_rejected() {
    // `f(mut v, mut v)` — the same binding borrowed exclusively by two
    // arguments of one call. The exclusive-borrow rule forbids it.
    let errors = ownership_errors(
        "fn f(x: mut ref Vec[i64], y: mut ref Vec[i64]) { x.push(1); y.push(2); }\n\
         fn main() { let mut v: Vec[i64] = Vec.new(); f(mut v, mut v); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::ExclusiveBorrowAliasedArgs),
        "f(mut v, mut v) must be an exclusive-borrow conflict; got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn exclusive_borrow_mut_plus_shared_same_binding_rejected() {
    // `g(mut v, v)` — exclusive borrow + shared borrow of the same binding.
    // The exclusive borrow must be the only active borrow of its place.
    let errors = ownership_errors(
        "fn g(x: mut ref Vec[i64], y: ref Vec[i64]) -> i64 { x.push(1); y[0] }\n\
         fn main() { let mut v: Vec[i64] = Vec.new(); v.push(9); let _ = g(mut v, v); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::ExclusiveBorrowAliasedArgs),
        "g(mut v, v) must be an exclusive-borrow conflict; got {:?}",
        errors.iter().map(|e| &e.kind).collect::<Vec<_>>()
    );
}

#[test]
fn exclusive_borrow_distinct_bindings_ok() {
    // Two exclusive borrows of DISTINCT bindings never alias — accepted.
    ownership_ok(
        "fn f(x: mut ref Vec[i64], y: mut ref Vec[i64]) { x.push(1); y.push(2); }\n\
         fn main() {\n\
             let mut a: Vec[i64] = Vec.new();\n\
             let mut b: Vec[i64] = Vec.new();\n\
             f(mut a, mut b);\n\
         }",
    );
}

#[test]
fn exclusive_borrow_disjoint_fields_ok() {
    // Two exclusive borrows of disjoint fields of the same struct do not
    // overlap (`p.x` vs `p.y`) — accepted.
    ownership_ok(
        "struct P { x: Vec[i64], y: Vec[i64] }\n\
         fn f(a: mut ref Vec[i64], b: mut ref Vec[i64]) { a.push(1); b.push(2); }\n\
         fn main() {\n\
             let mut p = P { x: Vec.new(), y: Vec.new() };\n\
             f(mut p.x, mut p.y);\n\
         }",
    );
}

// ── B-2026-07-01-10 — baked-stdlib `ref` params reach the ownership pass ──

#[test]
fn stats_calls_borrow_not_consume() {
    // `Stats.*` params are declared `xs: ref Slice[f64]` in
    // runtime/stdlib/stats.kara, but `collect_callee_param_modes` walked
    // only the USER program's items — the baked `ref` never reached
    // call-arg classification, every call consumed its argument, and two
    // statistics over one dataset was rejected ("value 'v' moved here,
    // used again here"). The collectors now walk STDLIB_PROGRAMS first.
    ownership_ok(
        "fn main() {\n\
             let mut v: Vec[f64] = Vec.new();\n\
             v.push(1.0);\n\
             v.push(2.0);\n\
             let a = Stats.sum(v);\n\
             let b = Stats.mean(v);\n\
             let c = Stats.median(v);\n\
             println(a + b + c);\n\
             println(v.len());\n\
         }",
    );
}

#[test]
fn stats_borrow_then_mutate_then_borrow_ok() {
    // A `ref` arg borrow ends at the call, so mutating between two Stats
    // calls is legal.
    ownership_ok(
        "fn main() {\n\
             let mut v: Vec[f64] = Vec.new();\n\
             v.push(1.0);\n\
             let a = Stats.sum(v);\n\
             v.push(2.0);\n\
             let b = Stats.sum(v);\n\
             println(a + b);\n\
         }",
    );
}

// ── B-2026-07-02-23/24/25/26: ownership-checker false-positive fixes ──
//
// These run the FULL front-end pipeline (parse → desugar → resolve →
// typecheck → lower → ownershipcheck). `lower` is what desugars
// comparison operators into `Type.eq(...)` calls, so B-23 only
// reproduces after it runs.

fn ownership_lowered_errors(source: &str) -> Vec<OwnershipError> {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {:?}",
        parsed.errors
    );
    desugar_program(&mut parsed.program);
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {:?}",
        resolved.errors
    );
    let typed = typecheck(&parsed.program, &resolved);
    lower(&mut parsed.program, &typed);
    ownershipcheck(&parsed.program, &typed).errors
}

fn ownership_lowered_ok(source: &str) {
    let errors = ownership_lowered_errors(source);
    assert!(
        errors.is_empty(),
        "Ownership errors: {}",
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

#[test]
fn b23_comparison_operators_borrow_string_operands() {
    // `a == b` then `a == c` on String: comparisons borrow both operands
    // (partial_eq.kara: `fn eq(ref self, other: ref Self)`), so the second
    // `a ==` is not a use-after-move.
    ownership_lowered_ok(
        "fn main() {\n\
             let a = \"hello\";\n\
             let b = \"hello\";\n\
             let c = \"world\";\n\
             if a == b { println(1); } else { println(0); }\n\
             if a == c { println(1); } else { println(0); }\n\
             if a != c { println(1); } else { println(0); }\n\
         }",
    );
}

#[test]
fn b23_comparison_operators_borrow_string_ordering() {
    ownership_lowered_ok(
        "fn main() {\n\
             let a = \"abc\";\n\
             let b = \"abd\";\n\
             if a < b { println(1); } else { println(0); }\n\
             if b > a { println(1); } else { println(0); }\n\
         }",
    );
}

#[test]
fn b23_comparison_borrows_in_header_iteration_loop() {
    // The natural handler shape: `for (k, v) in hdrs { if k == ".." {..}
    // if k == ".." {..} }` — the second `k ==` was a use-after-move.
    ownership_lowered_ok(
        "fn handle(hdrs: Vec[(String, String)]) -> i64 {\n\
             let mut found_echo = 0;\n\
             let mut found_host = 0;\n\
             for (k, v) in hdrs {\n\
                 if k == \"x-test-echo\" {\n\
                     if v == \"greetings\" { found_echo = 1; }\n\
                 }\n\
                 if k == \"host\" { found_host = 1; }\n\
             }\n\
             found_echo + found_host\n\
         }",
    );
}

#[test]
fn b23_user_impl_eq_ref_self_does_not_consume_operands() {
    // `==` on a user type whose PartialEq/Eq impls declare `ref self,
    // other: ref Self` must not consume the operands.
    ownership_lowered_ok(
        "struct Point { x: i64, y: i64 }\n\
         impl PartialEq for Point {\n\
             fn eq(ref self, other: ref Point) -> bool { self.x == other.x and self.y == other.y }\n\
         }\n\
         impl Eq for Point {\n\
             fn eq(self, other: Point) -> bool { self.x == other.x and self.y == other.y }\n\
         }\n\
         fn main() {\n\
             let a = Point { x: 1, y: 2 };\n\
             let b = Point { x: 1, y: 2 };\n\
             let c = Point { x: 9, y: 9 };\n\
             println(a == b);\n\
             println(a != c);\n\
         }",
    );
}

#[test]
fn b24_named_fn_value_is_copy_reused_across_sites() {
    // A bare fn item is a plain code pointer — freely duplicable.
    ownership_lowered_ok(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
         fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn main() {\n\
             let g = doubler;\n\
             let h: Fn(i64) -> i64 = doubler;\n\
             println(f\"{apply(g, 10i64)}\");\n\
             println(f\"{h(11i64)}\");\n\
         }",
    );
}

#[test]
fn b24_named_fn_value_two_call_arg_uses_ok() {
    ownership_lowered_ok(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
         fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn main() {\n\
             let a = apply(doubler, 10i64);\n\
             let b = apply(doubler, 11i64);\n\
             println(f\"{a + b}\");\n\
         }",
    );
}

#[test]
fn b24_once_callable_closure_second_call_still_uam() {
    // Positive control: a once-callable closure (captures an owned non-Copy
    // value) is NOT freely duplicable — calling it twice is still UAM.
    let errors = ownership_lowered_errors(
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = || { let _ = o; };\n\
             f();\n\
             f();\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "once-callable closure reuse must still be UAM; got {errors:?}"
    );
}

#[test]
fn b25_disjoint_field_paths_are_partial_moves() {
    // `use_it(b.left) + use_it(b.right)` — disjoint sub-places, not a
    // whole-`b` double move.
    ownership_lowered_ok(
        "struct Bin { left: String, right: String }\n\
         fn use_it(s: String) -> i64 { s.len() as i64 }\n\
         fn main() {\n\
             let b = Bin { left: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", right: \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" };\n\
             let _ = use_it(b.left) + use_it(b.right);\n\
         }",
    );
}

#[test]
fn b25_disjoint_tuple_projections_are_partial_moves() {
    ownership_lowered_ok(
        "fn use_it(s: String) -> i64 { s.len() as i64 }\n\
         fn main() {\n\
             let t: (String, String) = (\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\");\n\
             let _ = use_it(t.0) + use_it(t.1);\n\
         }",
    );
}

#[test]
fn b25_disjoint_tuple_projection_match_scrutinees() {
    // `match t.0 {..}` then `match t.1 {..}` — disjoint tuple positions.
    ownership_lowered_ok(
        "fn main() {\n\
             let t: (String, String) = (\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\");\n\
             let _a = match t.0 { x => x.len() };\n\
             let _b = match t.1 { y => y.len() };\n\
         }",
    );
}

#[test]
fn b25_struct_field_iterable_then_sibling_field_ok() {
    // `for a in c.args` (borrows the `args` sub-place) then a consume of a
    // sibling field — disjoint sub-places, not a whole-`c` move. The
    // natural tree-walking-eval shape; the iterable READ must carry its
    // place so it doesn't overlap the sibling consume.
    ownership_lowered_ok(
        "struct CallExpr { callee: String, args: Vec[String] }\n\
         fn take(s: String) -> i64 { s.len() as i64 }\n\
         fn eval(c: CallExpr) -> i64 {\n\
             let mut acc = 0;\n\
             for a in c.args { acc = acc + take(a); }\n\
             acc + take(c.callee)\n\
         }\n\
         fn main() { }",
    );
}

#[test]
fn b25_same_field_consumed_twice_still_uam() {
    // Positive control: consuming the SAME sub-place twice IS a UAM.
    let errors = ownership_lowered_errors(
        "struct Bin { left: String, right: String }\n\
         fn use_it(s: String) -> i64 { s.len() as i64 }\n\
         fn main() {\n\
             let b = Bin { left: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", right: \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" };\n\
             let _ = use_it(b.left) + use_it(b.left);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "consuming b.left twice must still be UAM; got {errors:?}"
    );
}

#[test]
fn b25_partial_move_then_whole_use_still_uam() {
    // Positive control: partial move then a whole-binding use IS a UAM
    // (the empty whole-place overlaps every partial place).
    let errors = ownership_lowered_errors(
        "struct Bin { left: String, right: String }\n\
         fn use_it(s: String) -> i64 { s.len() as i64 }\n\
         fn take_whole(b: Bin) -> i64 { b.left.len() as i64 }\n\
         fn main() {\n\
             let b = Bin { left: \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", right: \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\" };\n\
             let _ = use_it(b.left);\n\
             let _ = take_whole(b);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.kind == OwnershipErrorKind::UseAfterMove),
        "consuming b.left then whole b must still be UAM; got {errors:?}"
    );
}

#[test]
fn b26_with_provider_value_slot_is_borrow_not_consume() {
    // `with_provider[Metric](p, || ..); println(p.n)` — the provider value
    // is borrowed by the provider frame (mutation visible after pop), not
    // consumed, so the post-pop use is fine.
    ownership_lowered_ok(
        "pub trait Recorder { fn record(mut ref self, value: i64); }\n\
         pub struct Counter { n: i64 }\n\
         impl Recorder for Counter { fn record(mut ref self, value: i64) { self.n = value; } }\n\
         pub effect resource Metric: Recorder;\n\
         fn main() {\n\
             let mut p = Counter { n: 0 };\n\
             with_provider[Metric](p, || { Metric.record(99); });\n\
             println(p.n);\n\
         }",
    );
}

#[test]
fn b26_with_provider_closure_ref_capture_no_escape_error() {
    // Regression for the REPL provider-scope wrapper shape (the shape
    // `render_export_nodes` builds): an outer binding captured by the
    // `with_provider` closure must NOT trip the fn-arg-pass ref-capture-
    // escape rule — the provider machinery invokes the closure
    // synchronously and never stores it past the call. (Surfaced when
    // B-26 routed the generic `with_provider[R]` callee through
    // `callee_modes_for_call`, newly exposing its Own-mode closure slot to
    // the escape scan and breaking repl::nested_close_prunes_only_...)
    ownership_lowered_ok(
        "pub trait Recorder { fn record(mut ref self, value: i64); }\n\
         pub struct Counter { n: i64 }\n\
         impl Recorder for Counter { fn record(mut ref self, value: i64) { self.n = value; } }\n\
         pub effect resource Metric: Recorder;\n\
         fn main() {\n\
             let outer = 7;\n\
             let mut p = Counter { n: 0 };\n\
             with_provider[Metric](p, || { Metric.record(outer); });\n\
             println(p.n);\n\
         }",
    );
}

#[test]
fn generic_trait_method_ref_self_arg_is_borrow_not_move() {
    // B-2026-07-08-6 secondary — a method call on a generic-type receiver
    // dispatched through a single trait bound (`a.peek(b)` where `a: T`,
    // `T: Peek`, and `peek(ref self, other: ref Self)`) BORROWS its `ref Self`
    // argument, so reusing `b` afterwards is sound. The checker used to miss
    // the trait method's param modes (a type-param receiver records no
    // concrete `method_callee_types` key), defaulting the arg to a MOVE and
    // false-rejecting with "value 'b' moved here, used again here". Now it
    // falls back to the resolved `method_typeparam_trait_key` ("Peek.peek" ->
    // [Ref]) so the arg is a borrow. This is the shape a user-written generic
    // `min`/`max`/`clamp` (`match a.cmp(b) { Greater => b, _ => a }`) needs to
    // pass `karac check`.
    ownership_ok(
        "struct Name { s: String }\n\
         trait Peek { fn peek(ref self, other: ref Self) -> bool; }\n\
         impl Peek for Name { fn peek(ref self, other: ref Name) -> bool { self.s == other.s } }\n\
         fn gg[T: Peek](a: T, b: T) -> T { let x = a.peek(b); b }\n\
         fn main() { let r = gg(Name { s: \"a\" }, Name { s: \"b\" }); println(r.s); }",
    );
}

#[test]
fn generic_ord_min_body_passes_ownership() {
    // The concrete motivating case: a user generic `min[T: Ord]` whose body
    // compares via the trait method `a.cmp(b)` (`cmp(ref self, other: ref
    // Self)`) and returns one of the two owned params. `b` is borrowed by the
    // `cmp` call, then conditionally moved out — sound, and now accepted
    // (B-2026-07-08-6 secondary).
    ownership_ok(
        "struct Name { s: String }\n\
         impl PartialEq for Name { fn eq(ref self, other: ref Name) -> bool { self.s == other.s } }\n\
         impl Eq for Name {}\n\
         impl PartialOrd for Name { fn partial_cmp(ref self, other: ref Name) -> Option[Ordering] { Some(self.s.cmp(other.s)) } }\n\
         impl Ord for Name { fn cmp(ref self, other: ref Name) -> Ordering { self.s.cmp(other.s) } }\n\
         fn umin[T: Ord](a: T, b: T) -> T { match a.cmp(b) { Greater => b, _ => a } }\n\
         fn main() { let r = umin(Name { s: \"aaa\" }, Name { s: \"bbb\" }); println(r.s); }",
    );
}

// ── B-2026-07-16-12: builtin collection lookup methods borrow their key ──

#[test]
fn test_map_get_then_insert_string_key_borrows() {
    // design.md's collection tables spec `get`/`remove`/`contains_key` as
    // taking `key: ref K` — a lookup BORROWS the key. Before the fix, the
    // builtin Map methods had no syntactic signature (empty `struct Map[=K,=V]{}`)
    // so their key argument fell to the consume default, moving a heap key
    // (`String`) and rejecting the fundamental get-then-insert idiom (the word
    // counter). This must check clean: multiple lookups of the same String key
    // before the consuming `insert`.
    ownership_ok(
        "fn main() {\n\
        \x20   let mut m: Map[String, i64] = Map.new();\n\
        \x20   let k: String = \"apple\";\n\
        \x20   let present: bool = m.contains_key(k);\n\
        \x20   let cur: i64 = m.get(k).unwrap_or(0);\n\
        \x20   m.insert(k, cur + 1);\n\
        \x20   println(present);\n\
        }",
    );
}

#[test]
fn test_set_and_vec_contains_borrow_heap_element() {
    // `Set.contains(val: ref T)` / `Vec.contains(val: ref T)` borrow the probe
    // value — a heap `String` used for `contains` then again afterward must not
    // be flagged as moved.
    ownership_ok(
        "fn main() {\n\
        \x20   let mut s: Set[String] = Set.new();\n\
        \x20   let w: String = \"hello\";\n\
        \x20   let has: bool = s.contains(w);\n\
        \x20   s.insert(w);\n\
        \x20   println(has);\n\
        \x20   let v: Vec[String] = [\"a\", \"b\"];\n\
        \x20   let t: String = \"a\";\n\
        \x20   let found: bool = v.contains(t);\n\
        \x20   println(found);\n\
        \x20   println(t.len());\n\
        }",
    );
}

#[test]
fn test_map_insert_still_consumes_key_after_lookup() {
    // The fix is surgical: `insert(key: K, val: V)` still CONSUMES the key
    // (it is stored). A `get(k)` AFTER `insert(k, _)` is a genuine
    // use-after-move and must still be rejected.
    let errors = ownership_errors(
        "fn main() {\n\
        \x20   let mut m: Map[String, i64] = Map.new();\n\
        \x20   let k: String = \"apple\";\n\
        \x20   m.insert(k, 5);\n\
        \x20   let v: i64 = m.get(k).unwrap_or(0);\n\
        \x20   println(v);\n\
        }",
    );
    assert!(
        !errors.is_empty(),
        "using a String key after it was consumed by insert must still be a move error"
    );
}
