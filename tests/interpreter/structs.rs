//! structs, fields, SoA layouts, tuples, repr/ABI -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter structs::
//!
//! New fixtures about structs, fields, SoA layouts, tuples, repr/ABI belong in this file.

use super::*;

#[test]
fn test_extern_call_rejects_with_guidance_not_internal_error() {
    // B-2026-08-06-24: calling a foreign import under `--interp` used to fall
    // through to bare identifier evaluation and report
    //
    //   internal: name 'abs' resolved but has no binding at run time. This is a
    //   compiler bug (the resolver should have rejected or bound it) — please
    //   report it with the source.
    //
    // for a program that is not buggy, about a limitation that is a deliberate
    // design property (the tree-walk interpreter has no FFI boundary), while
    // blaming the resolver — which is right to bind a declared import. Every
    // FFI program hit it, and `karac run` is the first thing anyone following
    // the FFI docs reaches for.
    //
    // The assertions pin the three properties that made the old message wrong,
    // not its exact prose: the callee is NAMED, `karac build` is offered, and
    // the "compiler bug / report it" framing is gone.
    let errors = runtime_errors(
        "unsafe extern \"C\" { fn abs(n: i32) -> i32; }\n\
         fn main() { println(unsafe { abs(-5i32) }); }",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("abs")
            && e.message.contains("foreign import")
            && e.message.contains("karac build")),
        "an extern call must name the callee and point at `karac build`, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("compiler bug") || e.message.contains("internal:")),
        "an extern call is not a compiler bug and must not ask for a report, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_binding_shadowing_an_extern_name_still_runs() {
    // The refusal above is gated on the name not being bound, exactly like the
    // sibling MMIO-intrinsic arm. A local closure that shadows an imported name
    // is a real (if unwise) program and must still execute — otherwise the fix
    // would trade a bad diagnostic for a broken program.
    assert_eq!(
        run("unsafe extern \"C\" { fn abs(n: i32) -> i32; }\n\
             fn main() {\n\
                 let abs = |x| x + 100;\n\
                 println(abs(5));\n\
             }"),
        "105\n"
    );
}

#[test]
fn tuple_comparison_and_equality_in_the_interpreter() {
    // B-2026-08-27-33 -- the third member of the family above, and the same
    // shape as both: `type_supports_ord` / `type_supports_partial_eq` have
    // recursed through `Type::Tuple` since long before the row, so
    // `karac check` printed "All checks passed." on this file while the
    // interpreter died on the catch-all arm whose message claims the
    // typechecker reports this as a hard error. It does not, and did not.
    //
    // Ordering goes through `value_compare`, which is the TOTAL order
    // `karac_cmp_<T>` implements on the compiled side -- so the two backends
    // agree by construction rather than by convention, and this is the oracle
    // for `test_e2e_tuple_comparison_and_equality`.
    //
    // Equality is `Value`'s own `PartialEq` (IEEE element-wise), the same
    // comparator the `Vec` and `Slice` arms above use. That split is why the
    // float rows differ: `(1, 1.5, 2) == (1, 2.5, 2)` answers, while
    // `(1, 1.5) < (1, 2.5)` deliberately does NOT lower on either backend --
    // `value_compare` orders a bare float by `total_cmp` (NaN last) because
    // every one of its other callers is a sort key, and that is not what `<`
    // means on an `f64`.
    //
    // Arity is varied because the compiled twin has an arity-selective defect
    // the interpreter does not share (a three-scalar tuple collides with the
    // `{ptr, i64, i64}` String header there); keeping the two fixtures
    // row-for-row is what makes them checkable against each other.
    let src = "fn main() {
            let a = (1, 2);
            let b = (1, 3);
            println(f\"{a < b}\");
            println(f\"{b < a}\");
            println(f\"{a <= a}\");
            println(f\"{a >= a}\");
            println(f\"{a > b}\");
            println(f\"{a == b}\");
            println(f\"{a != b}\");
            let sa = (\"b\", 1);
            let sc = (\"c\", 0);
            println(f\"{sa < sc}\");
            let na = (1, (2, 3));
            let nb = (1, (2, 4));
            println(f\"{na < nb}\");
            println(f\"{na == nb}\");
            let t3 = (1, 2, 3);
            let u3 = (1, 2, 4);
            println(f\"{t3 < u3}\");
            println(f\"{t3 == u3}\");
            println(f\"{t3 == t3}\");
            let n3a = ((1, 2, 3), 4);
            let n3b = ((1, 2, 5), 4);
            println(f\"{n3a == n3b}\");
            println(f\"{n3a == n3a}\");
            let f3a = (1, 1.5, 2);
            let f3b = (1, 2.5, 2);
            println(f\"{f3a == f3b}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "true\nfalse\ntrue\ntrue\nfalse\nfalse\ntrue\n\
         true\n\
         true\nfalse\n\
         true\nfalse\ntrue\n\
         false\ntrue\n\
         false\n"
    );
}

// ── Structs ────────────────────────────────────────────────────

#[test]
fn test_struct_construction_and_field_access() {
    assert_eq!(
        run("struct Point { x: i64, y: i64 }\n\
             fn main() {\n\
                 let p = Point { x: 3, y: 4 };\n\
                 println(p.x + p.y);\n\
             }"),
        "7\n"
    );
}

#[test]
fn test_uppercase_local_binding_field_read() {
    // `F.value` on an uppercase local binding — the parser consumes the
    // uppercase-led dotted chain greedily into a `Path`, so the read lands
    // in the interpreter's `ExprKind::Path` arm (the value-binding walk),
    // not the `FieldAccess` arm. phase-8-stdlib-floor.md "Uppercase-receiver
    // field access" entry.
    assert_eq!(
        run("struct Foo { value: i64 }\n\
             fn main() {\n\
                 let F = Foo { value: 5 };\n\
                 let x: i64 = F.value;\n\
                 println(x);\n\
             }"),
        "5\n"
    );
}

#[test]
fn test_uppercase_local_binding_nested_field_read() {
    // `OUTER.inner.field` — a 3-segment `Path` walked field-by-field.
    assert_eq!(
        run("struct Inner { field: i64 }\n\
             struct Outer { inner: Inner }\n\
             fn main() {\n\
                 let OUTER = Outer { inner: Inner { field: 9 } };\n\
                 println(OUTER.inner.field);\n\
             }"),
        "9\n"
    );
}

#[test]
fn test_struct_method() {
    assert_eq!(
        run("struct Counter { value: i64 }\n\
             impl Counter {\n\
                 fn get(self) -> i64 { self.value }\n\
             }\n\
             fn main() {\n\
                 let c = Counter { value: 42 };\n\
                 println(c.get());\n\
             }"),
        "42\n"
    );
}

// ── Tuples ─────────────────────────────────────────────────────

#[test]
fn test_tuple_construction() {
    assert_eq!(
        run("fn main() {\n\
                 let t = (1, 2, 3);\n\
                 println(t.0 + t.1 + t.2);\n\
             }"),
        "6\n"
    );
}

// ── B-2026-07-03-7: `<` `<=` `>` `>=` on derived-Ord struct/enum ──

#[test]
fn test_ordered_operators_on_derived_ord_struct_and_enum() {
    // Pre-fix these operators were a hard type error on struct/enum operands
    // (no interp/codegen lowering). Now they lower through `value_compare`
    // (declaration order), so `karac run` and `karac build` agree.
    let output = run_no_errors(
        "#[derive(Eq, Ord)]\n\
         struct P { a: i64, b: i64 }\n\
         #[derive(Eq, Ord)]\n\
         enum Pri { Low, Med, High }\n\
         fn main() {\n\
             let x = P { a: 1, b: 2 };\n\
             let y = P { a: 1, b: 3 };\n\
             println(f\"{x < y}\");\n\
             println(f\"{y > x}\");\n\
             println(f\"{x <= x}\");\n\
             println(f\"{y < x}\");\n\
             println(f\"{Pri.Low < Pri.High}\");\n\
             println(f\"{Pri.High < Pri.Low}\");\n\
         }",
    );
    // a==a so P compares by b: 2<3; Low(0)<High(2).
    assert_eq!(output, "true\ntrue\ntrue\nfalse\ntrue\nfalse\n");
}

#[test]
fn test_plain_nested_struct_field_write() {
    // Regression: assigning to a *plain* (value-type) struct field through a
    // projection (`o.inner.x = v`, depth >= 2) was silently dropped — the
    // statement dispatch and `set_field` only handled bare-identifier targets,
    // so the write no-op'd. Now the parent copy is updated and written back up
    // the place chain. (Pre-existing; surfaced by Tangle dogfooding.)
    assert_eq!(
        run("struct Inner { x: i64 }\n\
             struct Outer { inner: Inner }\n\
             fn main() {\n\
                 let mut o = Outer { inner: Inner { x: 1 } };\n\
                 o.inner.x = 99;\n\
                 println(o.inner.x);\n\
             }"),
        "99\n"
    );
}

#[test]
fn test_plain_nested_struct_field_write_three_levels() {
    // Write-back composes to arbitrary depth.
    assert_eq!(
        run("struct A { x: i64 }\n\
             struct B { a: A }\n\
             struct C { b: B }\n\
             fn main() {\n\
                 let mut c = C { b: B { a: A { x: 1 } } };\n\
                 c.b.a.x = 42;\n\
                 println(c.b.a.x);\n\
             }"),
        "42\n"
    );
}

#[test]
fn test_compound_assign_on_field_and_nested() {
    // Compound assignment (`+=`) previously only handled bare-identifier
    // targets; field and nested-field targets were silently dropped. Now it
    // routes through the same place-assignment path.
    assert_eq!(
        run("struct Inner { x: i64 }\n\
             struct Outer { mut count: i64, inner: Inner }\n\
             fn main() {\n\
                 let mut o = Outer { count: 0, inner: Inner { x: 5 } };\n\
                 o.count += 10;\n\
                 o.inner.x += 100;\n\
                 println(o.count);\n\
                 println(o.inner.x);\n\
             }"),
        "10\n105\n"
    );
}

#[test]
fn test_weak_field_alive_yields_some() {
    // Per design.md § Shared Types — Weak references: a `weak` field
    // read is the upgrade point. While a strong holder of the referent
    // is in scope, the upgrade succeeds and yields `Some(strong_ref)`.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn main() {\n\
                 let p = Parent { id: 7 };\n\
                 let c = Child { id: 2, parent: p };\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "7\n"
    );
}

#[test]
fn test_weak_field_reassignment_restores_some() {
    // A `mut weak` field can be reassigned after construction. The
    // assignment auto-downgrades the strong rhs. Reading after the
    // first assignment's referent dies yields None; assigning a fresh
    // live parent restores Some.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn main() {\n\
                 let p1 = Parent { id: 1 };\n\
                 let c = Child { id: 9, parent: p1 };\n\
                 let p2 = Parent { id: 2 };\n\
                 c.parent = p2;\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "2\n"
    );
}

#[test]
fn test_e2e_struct_with_methods() {
    assert_eq!(
        run("struct Rect { width: i64, height: i64 }\n\
             impl Rect {\n\
                 fn area(self) -> i64 { self.width * self.height }\n\
                 fn is_square(self) -> bool { self.width == self.height }\n\
             }\n\
             fn main() {\n\
                 let r = Rect { width: 3, height: 4 };\n\
                 println(r.area());\n\
                 println(r.is_square());\n\
                 let s = Rect { width: 5, height: 5 };\n\
                 println(s.is_square());\n\
             }"),
        "12\nfalse\ntrue\n"
    );
}

// ── Edge Cases: Scoping ────────────────────────────────────────

#[test]
fn test_nested_scopes_shadow() {
    assert_eq!(
        run("fn main() {\n\
                 let x = 1;\n\
                 let y = {\n\
                     let x = 2;\n\
                     x\n\
                 };\n\
                 println(x);\n\
                 println(y);\n\
             }"),
        "1\n2\n"
    );
}

#[test]
fn test_nested_if_else() {
    assert_eq!(
        run("fn classify(x: i64) -> i64 {\n\
                 if x > 100 {\n\
                     3\n\
                 } else if x > 10 {\n\
                     2\n\
                 } else if x > 0 {\n\
                     1\n\
                 } else {\n\
                     0\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(classify(200));\n\
                 println(classify(50));\n\
                 println(classify(5));\n\
                 println(classify(-1));\n\
             }"),
        "3\n2\n1\n0\n"
    );
}

// ── Edge Cases: Struct Patterns ────────────────────────────────

#[test]
fn test_struct_destructuring_in_let() {
    assert_eq!(
        run("struct Point { x: i64, y: i64 }\n\
             fn main() {\n\
                 let p = Point { x: 10, y: 20 };\n\
                 let Point { x, y } = p;\n\
                 println(x + y);\n\
             }"),
        "30\n"
    );
}

#[test]
fn test_tuple_destructuring_in_let() {
    assert_eq!(
        run("fn main() {\n\
                 let t = (1, 2, 3);\n\
                 let (a, b, c) = t;\n\
                 println(a + b + c);\n\
             }"),
        "6\n"
    );
}

// ── Edge Cases: Method + Enum Interaction ──────────────────────

#[test]
fn test_static_method_constructor() {
    assert_eq!(
        run("struct Vec2 { x: i64, y: i64 }\n\
             impl Vec2 {\n\
                 fn new(x: i64, y: i64) -> Vec2 { Vec2 { x: x, y: y } }\n\
                 fn dot(self, other: Vec2) -> i64 { self.x * other.x + self.y * other.y }\n\
             }\n\
             fn main() {\n\
                 let a = Vec2.new(3, 4);\n\
                 let b = Vec2.new(1, 2);\n\
                 println(a.dot(b));\n\
             }"),
        "11\n"
    );
}

#[test]
fn test_interpreter_nested_labeled_break_outer() {
    // Mirror of codegen latent-bug regression gate: `outer: while {
    // inner: while { break outer; } }` exits the outer loop. Pre-slice
    // interpreter already routed through `ControlFlow::Break.label`
    // correctly, but the test pins the contract to prevent a future
    // regression that flattens the label match.
    assert_eq!(
        run("fn main() {\n\
             let mut count = 0;\n\
             outer: while true {\n\
                 inner: while true {\n\
                     count = count + 1;\n\
                     break outer ();\n\
                 }\n\
                 count = count + 100;\n\
             }\n\
             println(count);\n\
         }"),
        "1\n"
    );
}

// ── F64/F32 Total-Order Types ─────────────────────────────────

// B-2026-08-11-8: these two asserted `F64(3.14)` / `F32(2.5…)`, a rendering
// that no CHECKED program could ever observe — both `println(x)` and `f"{x}"`
// rejected `F64` at typecheck ("does not implement Display"), and this
// harness's `run()` bypasses that gate. Giving the wrapper Display meant
// choosing a rendering, and it renders as the inner float so that wrapping a
// value for its `Ord` contract does not change how it prints. Codegen's
// `synth_display.rs` arm reuses the same shortest-round-trip float formatter,
// so the two backends cannot drift.
#[test]
fn test_f64_from_constructor() {
    let output = run("fn main() { let x = F64.from(3.14); println(x); }");
    assert_eq!(output, "3.14\n");
}

#[test]
fn test_f32_from_constructor() {
    let output = run("fn main() { let x = F32.from(2.5); println(x); }");
    assert!(output.starts_with("2.5"), "got {output:?}");
}

#[test]
fn test_f16_bf16_from_constructor() {
    // B-2026-08-11-8: renders as the inner float now, like its F32/F64
    // siblings — see `test_f64_from_constructor` for why the old
    // `F16(2.5)` form was never observable from a checked program.
    let f16 = run("fn main() { let x = F16.from(2.5); println(x); }");
    assert!(f16.starts_with("2.5"), "got {f16:?}");
    let bf16 = run("fn main() { let x = Bf16.from(3.25); println(x); }");
    assert!(bf16.starts_with("3.25"), "got {bf16:?}");
}

#[test]
fn test_with_provider_nested_same_resource_inner_shadows_outer_restored_on_pop() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             with_provider[UserDB](Db { tag: 1 }, || {
                 println(UserDB.id());
                 with_provider[UserDB](Db { tag: 2 }, || {
                     println(UserDB.id());
                 });
                 println(UserDB.id());
             });
         }");
    assert_eq!(output, "1\n2\n1\n");
}

#[test]
fn test_file_append_constructor_writes_at_end() {
    // `File.append` opens in append mode (positions writes at end of
    // file, creating it if absent). Two consecutive appends should
    // produce concatenated contents.
    let tmp = std::env::temp_dir().join("karac_test_file_append.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             let first = [97u8, 98u8];
             let second = [99u8, 100u8];
             match File.append(\"{path}\") {{
                 Ok(f) => {{ f.write(first[0..2]); }}
                 Err(_) => println(\"append1 err\"),
             }}
             match File.append(\"{path}\") {{
                 Ok(f) => {{ f.write(second[0..2]); }}
                 Err(_) => println(\"append2 err\"),
             }}
         }}"
    );
    let _ = run_no_errors(&src);
    let written = std::fs::read(&tmp).expect("temp read");
    assert_eq!(written, b"abcd");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_distinct_constructor_passed_through_function() {
    // A distinct value round-trips through a function call and back out
    // via `.raw()` — the wrapper is purely a type-level distinction.
    let output = run_no_errors(
        "distinct type UserId = i64;\n\
         fn identity(id: UserId) -> UserId { id }\n\
         fn main() {\n\
             let u = identity(UserId(7));\n\
             println(u.raw());\n\
         }",
    );
    assert_eq!(output, "7\n");
}

#[test]
fn test_distinct_where_constructor_runtime_holds() {
    // Combined `distinct type Even = i64 where self % 2 == 0`: a runtime
    // argument that satisfies the predicate constructs successfully.
    let output = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn mk(n: i64) -> Even { Even(n) }\n\
         fn main() { println(mk(8).raw()); }",
    );
    assert_eq!(output, "8\n");
}

#[test]
fn test_distinct_where_constructor_runtime_fails() {
    // A runtime argument that violates the predicate faults with a
    // `contract violated` runtime error.
    let errors = runtime_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn mk(n: i64) -> Even { Even(n) }\n\
         fn main() { println(mk(7).raw()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault, got: {errors:?}"
    );
}

#[test]
fn test_tracing_log_event_with_fields_and_span() {
    let output = run(r#"fn main() {
         let e = LogEvent.info("started")
             .with_field("user_id", "42")
             .with_field("ip", "127.0.0.1")
             .in_span(5);
         println(e.level);
         println(e.message);
         println(e.fields.len());
         println(e.span_id);
     }"#);
    assert_eq!(output, "info\nstarted\n2\n5\n");
}

#[test]
fn test_arena_get_struct_field_access() {
    // The primary arena use case — structs (AST nodes / ECS rows) bump-
    // allocated and read back by field. `get` returns `ref Node`; field
    // access auto-derefs, so `n.val` works without an explicit `*`.
    let output = run(r#"struct Node { val: i64, next: i64 }
         fn main() {
             let a: Arena[Node] = Arena.new();
             let r = a.push(Node { val: 7, next: 99 });
             let n = a.get(r);
             println(n.val);
             println(n.next);
         }"#);
    assert_eq!(output, "7\n99\n");
}

#[test]
fn test_derive_default_struct_primitives() {
    // `#[derive(Default)]` synthesizes `Config.default()` field-by-field:
    // ints/floats → 0, bool → false, String → "". Book appendix C example.
    let output = run(r#"
#[derive(Default)]
struct Config {
    timeout_ms: i64,
    ratio: f64,
    verbose: bool,
    name: String,
    tag: char,
}

fn main() {
    let c = Config.default();
    println(c.timeout_ms);
    println(c.ratio);
    println(c.verbose);
    println(c.name);
    println(c.tag as i64);
}
"#);
    assert_eq!(output, "0\n0\nfalse\n\n0\n");
}

#[test]
fn test_derive_default_nested_struct() {
    // A derive-Default field whose type also derives Default recurses
    // through `Inner.default()` in declaration order.
    let output = run(r#"
#[derive(Default)]
struct Inner { x: i64, y: i64 }

#[derive(Default)]
struct Outer { a: Inner, scale: i64 }

fn main() {
    let o = Outer.default();
    println(o.a.x);
    println(o.a.y);
    println(o.scale);
}
"#);
    assert_eq!(output, "0\n0\n0\n");
}

/// The bare-`T` spelling, which only the interpreter answers today
/// (B-2026-08-27-41; the compiled half waits on B-2026-08-27-40).
///
/// This is the shape the row was filed for: `fn smaller[T: Ord](a: T, b: T)
/// -> bool { a < b }` does not lower to a comparison — it lowers to `T.cmp` —
/// so a tuple argument used to die at "method 'cmp' not found on type
/// 'unknown'" even though `(1, 2) < (1, 3)` written directly had worked since
/// B-2026-08-27-33. `karac check` passed the whole time, which is what made it
/// a run-vs-build hole rather than a rejection.
#[test]
fn tuple_cmp_through_a_bare_type_parameter() {
    let out = run_no_errors(
        r#"
fn smaller[T: Ord](a: T, b: T) -> bool { return a < b; }

fn main() {
    println(f"{smaller((1, 2), (1, 3))}");
    println(f"{smaller((1, 3), (1, 2))}");
    println(f"{smaller((1, 2), (1, 2))}");
    println(f"{smaller(("a", 9), ("b", 0))}");
    println(f"{smaller(1, 2)}");
}
"#,
    );
    assert_eq!(out, "true\nfalse\nfalse\ntrue\ntrue\n");
}

#[test]
fn test_contract_constructor_non_self_return_not_checked() {
    // A static associated function returning some *other* type (`-> i64`) is
    // not a constructor — its return value must NOT be invariant-checked,
    // even though the type has an invariant and the value would violate it.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn answer() -> i64 { 0 - 9 } }\n\
         fn main() { let _x = Counter.answer(); }",
    );
    assert!(
        errors.is_empty(),
        "a non-Self-returning assoc fn must not be invariant-checked, got: {errors:?}"
    );
}

#[test]
fn test_f16_arithmetic_results_are_representable_in_f16() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let n: i64 = 1;\n\
             \x20   let one: f32 = n as f32;\n\
             \x20   let big: f16 = (one * 65504.0f32) as f16;\n\
             \x20   let three: f16 = (one * 3.0f32) as f16;\n\
             \x20   let a: f16 = (one * 3.5f32) as f16;\n\
             \x20   let b: f16 = (one * 1.25f32) as f16;\n\
             \x20   let p: f16 = (one * 0.1f32) as f16;\n\
             \x20   let q: f16 = (one * 0.3f32) as f16;\n\
             \x20   let tiny: f16 = (one * 0.00006103515625f32) as f16;\n\
             \x20   println(f\"ovf {big * three} {big + three}\");\n\
             \x20   println(f\"div {a / b} {p / q}\");\n\
             \x20   println(f\"inx {p + q} {p * q} {p - q}\");\n\
             \x20   println(f\"unf {tiny * tiny}\");\n\
             \x20   let mut s: f16 = p;\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 8 {\n\
             \x20       s = s + p;\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(f\"acc {s}\");\n\
             \x20   println(f\"mth {big.sqrt()} {a.exp()} {a.ln()} {a.floor()}\");\n\
             \x20   let u: Vector[f16, 4] = Vector[f16, 4](big, p, a, tiny);\n\
             \x20   let v: Vector[f16, 4] = Vector[f16, 4](three, q, b, tiny);\n\
             \x20   let w = u * v;\n\
             \x20   let z = u + v;\n\
             \x20   println(f\"vec {w[0]} {w[1]} {w[3]} {z[1]}\");\n\
             }\n"),
        // 65504 is the largest finite f16, so the product overflows and the
        // sum rounds back down to it; tiny*tiny underflows past the smallest
        // subnormal. Pre-fix, wasm gave a finite 196512, 65507, and 2^-28.
        "ovf inf 65504\n\
         div 2.80078125 0.333251953125\n\
         inx 0.39990234375 0.029998779296875 -0.2000732421875\n\
         unf 0\n\
         acc 0.900390625\n\
         mth 255.875 33.125 1.2529296875 3\n\
         vec inf 0.029998779296875 0 0.39990234375\n"
    );
}

#[test]
fn struct_equality_structural() {
    let output = run(r#"
#[derive(Eq)]
struct Point { x: i64, y: i64 }

fn main() {
    let p = Point { x: 1, y: 2 };
    let q = Point { x: 1, y: 2 };
    let r = Point { x: 1, y: 3 };
    println(f"{p == q}");
    println(f"{p == r}");
    println(f"{p != r}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\n");
}

// ── size_of[T]() / align_of[T]() — interpreter parity with codegen ──
//
// Same gap family as offset_of above: the interpreter had no intercept
// for the layout-query call shapes, so `karac run` panicked ("variable
// 'size_of' not found") on programs `karac build` compiled fine. All
// expected values below are build-verified (alloc size / ABI align of
// the lowered LLVM type).

#[test]
fn size_of_and_align_of_struct() {
    let out = run_no_errors(
        "struct Point { x: i64, y: i64 }\n\
         fn main() { println(size_of[Point]()); println(align_of[Point]()); }",
    );
    assert_eq!(out, "16\n8\n");
}

#[test]
fn size_of_padded_struct_and_primitives() {
    // Mixed pads to 32 (see offset_of_mixed_alignment_padding); i32 is
    // 4/4; bool is the 1-byte i1 slot; char is a 4-byte scalar; String
    // is the 24-byte {ptr,len,cap} ABI aggregate.
    let out = run_no_errors(
        "struct Mixed { a: bool, b: i32, c: i8, d: i64, e: i16 }\n\
         fn main() {\n\
         \x20   println(size_of[Mixed]());\n\
         \x20   println(align_of[Mixed]());\n\
         \x20   println(size_of[i32]());\n\
         \x20   println(align_of[i32]());\n\
         \x20   println(size_of[bool]());\n\
         \x20   println(size_of[char]());\n\
         \x20   println(size_of[String]());\n\
         }",
    );
    assert_eq!(out, "32\n8\n4\n4\n1\n4\n24\n");
}

/// B-2026-08-25-35 — the end-to-end payoff: a user struct in a `PriorityQueue`.
///
/// Needs BOTH halves of that row's fix. The bound gate above lets `Task` in at
/// all; then `outranks` compares two INDEXED elements (`self.xs[i] > self.xs[j]`),
/// and resolving that element's type name walked the DECLARED field type — `Vec[T]`
/// on `struct PriorityQueue[=T]` — yielding `T`, the impl's type PARAMETER rather
/// than the monomorph's argument. `T` names no struct, so the ordered-comparison
/// dispatch declined and codegen failed with "Unsupported struct binary op: Gt".
/// Same class as B-2026-08-25-28: a type expr left in terms of the impl's parameter
/// inside a monomorph.
///
/// Both directions and `peek`/`pop`/`len` appear because `outranks` is the one
/// branch that differs between them, and because a comparison that silently read
/// index 0 without the heap property holding would still look right on a min-first
/// queue of three.
///
/// Twin of `tests/codegen.rs`'s `e2e_priority_queue_of_a_derived_ord_struct`.
/// PARITY test, not the regression oracle — see the sibling above for why the
/// tree-walk harness cannot fail on a bound rejection. Its codegen twin and
/// `tests/typechecker.rs`'s `derived_ord_struct_is_accepted_by_priority_queue`
/// are the ones that go red on the unfixed tree.
#[test]
fn test_priority_queue_of_a_derived_ord_struct() {
    let out = run(r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Task { pri: i64, id: i64 }
fn main() {
    let mut q: PriorityQueue[Task] = PriorityQueue.new();
    q.push(Task { pri: 3, id: 30 });
    q.push(Task { pri: 1, id: 10 });
    q.push(Task { pri: 2, id: 20 });
    match q.peek() { Some(t) => { println(t.id); } None => {} }
    println(q.len());
    while q.len() > 0 {
        match q.pop() { Some(t) => { println(t.id); } None => {} }
    }
    let mut m: PriorityQueue[Task] = PriorityQueue.max_first();
    m.push(Task { pri: 3, id: 30 });
    m.push(Task { pri: 1, id: 10 });
    match m.peek() { Some(t) => { println(t.id); } None => {} }
}
"#);
    assert_eq!(out, "10\n3\n10\n20\n30\n30\n");
}

#[test]
fn gpu_reduce_over_a_temporary_buffer_field_diagnoses_rather_than_panics() {
    // GPU-SLIP-4b-3b. A resident field reduction is compiled-only like the
    // `gpu.upload` that made the buffer, and it must SAY so on every receiver
    // shape.
    //
    // The temporary receiver is the one that had no statement boundary to stop
    // at: the nested `gpu.upload` records its error and yields `Unit`, then the
    // field access lands on a non-struct receiver and trips the invariant
    // assert there — an ICE, not a diagnostic. The `let`-bound form only looked
    // fine because evaluation halted at its own statement first.
    for receiver in ["gpu.upload(v)", "gpu.dispatch(step, gpu.upload(v))"] {
        let errors = runtime_errors(&format!(
            "struct P {{ a: f32 }}\n\
             #[gpu]\n\
             fn step(p: P) -> P {{ p }}\n\
             fn main() {{\n\
                 let mut v: Vec[P] = Vec.new();\n\
                 v.push(P {{ a: 1.0 }});\n\
                 println(f\"{{gpu.sum(({receiver}).a)}}\");\n\
             }}"
        ));
        assert!(
            !errors.is_empty(),
            "a resident field reduction over `{receiver}` must produce a runtime error"
        );
        assert!(
            errors[0].message.contains("compiled path"),
            "expected the compiled-only diagnostic for `{receiver}`, got: {:?}",
            errors[0].message
        );
    }
}

#[test]
fn gpu_reduce_over_an_ordinary_struct_field_still_runs_on_the_host() {
    // The guard above keys on the typechecker's resident-field table, not on
    // "the argument is a field access" — so a field that holds a host
    // `Vec[f32]` is an ordinary reduction and must still compute. Reducing
    // `gpu.sum(rec.values)` to a compiled-only error would be the obvious way
    // to get this wrong.
    let out = run("struct Rec { values: Vec[f32] }\n\
         fn main() {\n\
             let r = Rec { values: [1.0, 2.0, 3.5] };\n\
             println(f\"{gpu.sum(r.values)}\");\n\
         }");
    assert_eq!(out.trim(), "6.5", "host field reduction must still run");
}

/// B-2026-09-03-14 — THE THREE ARMS B-2026-09-02-43's OWNER MASK DID NOT REACH.
///
/// That row taught `finish_place_source_tuple_destructure` to mask the elements whose
/// leaf took a body out of the owner's `__karac_dropbodies_*` walk, and recorded them
/// from ONE site: the enum / nested-struct BINDING arm. Three other spellings hand a
/// body away at the same statement and recorded nothing, so the owner kept running it.
/// Measured against `--interp` on the commit that fixed -43:
///
///   * `wild` / `wildidx` — `let (_, k) = h.pe;`. The wildcard arm runs the discarded
///     element's body ON THE SPOT and never cap-zeroes (the aggregate keeps the MEMORY,
///     by design), so this one's defect looks unlike the row's: TWO bodies both reading
///     a LIVE value, rather than a husk and a live one. `wildidx` is the same shape with
///     the wildcard at index 1, since the recorded index has to be the element's own.
///   * `nested` — `let ((r, a), b) = h.pe;`. The recursion cap-zeroes the inner leaf it
///     takes, but the call site discarded the inner set outright, so nothing recorded
///     the OUTER element and the owner's walk still descended into the field: `dR4//0`
///     before the live read. -43's defect exactly, one level down.
///   * `owndrop` — a parent with its own `impl Drop`. It has NO per-binding bodies
///     action to replace: it runs its field bodies from inside `karac_drop_<T>`, the
///     type-level wrapper registered as a separate `OwnWrapper` action
///     (B-2026-09-01-40), so the disarm's replace/suppress pair found nothing.
///     `dHd5 dR5//0 b5/t5 dR5/t5/1` against the interpreter's three lines.
///
/// `wildopt` IS THE OVER-REACH CONTROL, and it is why the wildcard arm asks a question
/// rather than masking unconditionally. Over `(i64, Option[R])` the discard helper
/// DECLINES — its `Option`/`Result` handling is not the parent walk's — so the body it
/// would silence is the only one there is. The signal is
/// `run_discarded_leaf_user_drop_bodies`' `ran_bodies`, and the single `bool` it used to
/// return meant `took_memory`, which is ALWAYS false at a call site passing
/// `free_memory: false`; gating on that silently disabled the whole `wild` fix instead.
/// The helper now reports both answers separately.
///
/// `plain` and `param` are the unchanged legs: -43's own shape stays at one live body,
/// and a by-value param source keeps the caller-retained body fired after the callee
/// returns. Every cell renders `self.tag` and `self.xs.len()`, because on an
/// `R { id: i64 }` a husk and a live value print the same thing and a count-only
/// assertion cannot tell them apart.
///
/// `d1` (the `owndrop` cell) carries `#[allow(partial_move_of_drop_struct)]` since
/// B-2026-09-01-43: moving a tuple field out of a parent that declares its own `Drop`
/// is exactly the shape design.md § Part 8 `Drop` rejects, so at `Deny` the cell no
/// longer compiles without the opt-out. Keeping it is deliberate — the drop placement
/// it pins stays reachable through that attribute, so the coverage still guards
/// programs someone can write.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_destructure_owner_mask_reaches_the_remaining_arms`, pinned to the same string.
/// All four surfaces agree here, which is the point: three of these cells were compiled-
/// only defects and this backend was already correct on every one of them.
#[test]
fn test_destructure_owner_mask_reaches_the_remaining_arms() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }

struct H  { pe: (R, i64) }
struct Hi { pe: (i64, R) }
struct Hw { pe: (i64, Option[R]) }
struct Hn { pe: ((R, i64), i64) }
struct Hd { pe: (R, i64), n: i64 }
impl Drop for Hd { fn drop(mut ref self) { println(f"dHd{self.n}") } }

fn mk(id: i64) -> R { let mut v: Vec[i64] = Vec.new(); v.push(id); return R { id: id, tag: f"t{id}", xs: v } }

fn w1() { let h = H  { pe: (mk(1), 0) };            let (_, k) = h.pe;      println("  end") }
fn w2() { let h = Hi { pe: (0, mk(2)) };            let (k, _) = h.pe;      println("  end") }
fn w3() { let h = Hw { pe: (0, Option.Some(mk(3))) }; let (k, _) = h.pe;    println("  end") }
fn n1() { let h = Hn { pe: ((mk(4), 0), 1) };       let ((r, a), b) = h.pe; println(f"  b{r.id}/{r.tag}"); println("  end") }
#[allow(partial_move_of_drop_struct)]
fn d1() { let h = Hd { pe: (mk(5), 0), n: 5 };      let (r, k) = h.pe;      println(f"  b{r.id}/{r.tag}"); println("  end") }
fn c1() { let h = H  { pe: (mk(6), 0) };            let (r, k) = h.pe;      println(f"  b{r.id}/{r.tag}"); println("  end") }
fn c2(h: H) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println("  end") }

fn main() {
    println("wild");     w1();                      println("wild end")
    println("wildidx");  w2();                      println("wildidx end")
    println("wildopt");  w3();                      println("wildopt end")
    println("nested");   n1();                      println("nested end")
    println("owndrop");  d1();                      println("owndrop end")
    println("plain");    c1();                      println("plain end")
    println("param");    c2(H { pe: (mk(7), 0) });  println("param end")
    println("done")
}
"#),
        r#"wild
dR1/t1/1
  end
wild end
wildidx
dR2/t2/1
  end
wildidx end
wildopt
dR3/t3/1
  end
wildopt end
nested
  b4/t4
dR4/t4/1
  end
nested end
owndrop
dHd5
  b5/t5
dR5/t5/1
  end
owndrop end
plain
  b6/t6
dR6/t6/1
  end
plain end
param
  b7/t7
  end
dR7/t7/1
param end
done
"#
    );
}

/// Twin of `tests/codegen.rs`'s
/// `e2e_projection_source_struct_destructure_hands_each_leaf_its_body`, pinned
/// to the same string.
///
/// This side was ALREADY correct on every cell — the measurement that made
/// B-2026-09-04-2 a codegen-only fix rather than a which-backend-is-right
/// question. It is pinned here so a later change cannot move the interpreter
/// onto codegen's old placement and call the pair "agreed".
///
/// B-2026-09-04-1 — `local` (the owned-local source) now runs the leaf's body,
/// `dR102`, at the destructure. The PROJECTION cells (`res`, `two`, `live`)
/// followed with B-2026-09-04-21: the leaf now owns the field there too
/// (transferred on a move, copied when the root is read again), so each
/// runs the unused leaf's body at the destructure.
#[test]
fn test_projection_source_struct_destructure_hands_each_leaf_its_body() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }

struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct HoPln { a: R, b: R }
struct HoStr { a: R, b: String }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct WrapP { inner: HoPln }
struct WrapS { inner: HoStr }
struct Outer { h: WrapR }

fn cell_res()   { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_local() { let h = HoRes { a: mk(2), b: Result.Ok(mk(102)) };                  let HoRes { a, b } = h;       println(f"  rd{a.id}") }
fn cell_opt()   { let w = WrapO { inner: HoOpt { a: mk(3), b: Option.Some(mk(103)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_pln()   { let w = WrapP { inner: HoPln { a: mk(4), b: mk(104) } };            let HoPln { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_str()   { let w = WrapS { inner: HoStr { a: mk(5), b: "plain" } };            let HoStr { a, b } = w.inner; println(f"  rd{a.id}") }
fn cell_two()   { let g = Outer { h: WrapR { inner: HoRes { a: mk(6), b: Result.Ok(mk(106)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}") }
fn cell_live()  { let w = WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}"); println(f"  w{w.inner.a.id}") }
fn cell_nodest(){ let w = WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } }; println(f"  rd{w.inner.a.id}") }
fn cell_move()  { let w = WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } }; let h = w.inner; println(f"  rd{h.a.id}") }

fn main() {
    println("res");    cell_res()
    println("local");  cell_local()
    println("opt");    cell_opt()
    println("pln");    cell_pln()
    println("str");    cell_str()
    println("two");    cell_two()
    println("live");   cell_live()
    println("nodest"); cell_nodest()
    println("move");   cell_move()
    println("done")
}
"#);
    assert_eq!(
        out,
        r#"res
dR101/t101
  rd1
dR1/t1
local
dR102/t102
  rd2
dR2/t2
opt
dR103/t103
  rd3
dR3/t3
pln
dR104/t104
  rd4
dR4/t4
str
  rd5
dR5/t5
two
dR106/t106
  rd6
dR6/t6
live
dR107/t107
  rd7
dR7/t7
  w7
nodest
  rd8
dR108/t108
dR8/t8
move
  rd9
dR109/t109
dR9/t9
done
"#
    );
}

/// Twin of `tests/codegen.rs`'s
/// `e2e_projection_source_tuple_destructure_is_a_view`, pinned to the same
/// string.
#[test]
fn test_projection_source_tuple_destructure_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H  { pe: (R, i64) }
struct Hs { pe: (R, i64), name: String }
struct G  { h: H }

fn f1(h: H)   { let (r, k) = h.pe;   let m = r; println(f"  b{m.id}") }
fn f2(hs: Hs) { let (r, k) = hs.pe;  let m = r; println(f"  b{m.id} {hs.name}") }
fn f3(g: G)   { let (r, k) = g.h.pe; let m = r; println(f"  b{m.id}") }
fn f4(h: H)   { let (r, k) = h.pe;   println(f"  b{r.id}") }
fn f5(h: H)   { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") }
fn f6(h: ref H) { println(f"  b{h.pe.0.id}") }

fn main() {
    println("plain");    f1(H  { pe: (R { id: 1 }, 0) });                println("plain end")
    println("ownstr");   f2(Hs { pe: (R { id: 2 }, 0), name: "n" });     println("ownstr end")
    println("twohop");   f3(G  { h: H { pe: (R { id: 3 }, 0) } });       println("twohop end")
    println("norebind"); f4(H  { pe: (R { id: 4 }, 0) });                println("norebind end")
    println("rebind");   f5(H  { pe: (R { id: 5 }, 0) });                println("rebind end")
    println("refparam"); let h6 = H { pe: (R { id: 6 }, 0) }; f6(h6);    println("refparam end")
    println("done")
}
"#),
        r#"plain
  b1
dR1
plain end
ownstr
  b2 n
dR2
ownstr end
twohop
  b3
dR3
twohop end
norebind
  b4
dR4
norebind end
rebind
  b5
dR5
rebind end
refparam
  b6
dR6
refparam end
done
"#
    );
}

/// B-2026-09-02-44 — a projection destructure whose root INHERITED view-ness
/// from an owned param (`let h2 = h; let (r, k) = h2.pe;`) binds views too, exactly
/// as one rooted at the param itself does since B-2026-09-02-40.
///
/// The concept was already on both sides and already transitive: `let h2 = h;`
/// writes `h2` into codegen's `param_view_locals` and the interpreter's
/// `owned_param_names_stack`, which is why `let h2 = h; let x = h2.pe;` and
/// `let t2 = t; let x = t2.0;` were correct before this. The destructure gate was
/// the ONE place asking the narrower question — "is the root a PARAMETER" —
/// against `current_fn_param_names` rather than the union its own
/// `expr_is_param_view` reads.
///
/// WHAT THE FILING ROW GOT WRONG, and why these cells are pinned here rather than
/// only as `rebind` in `test_projection_source_tuple_destructure_is_a_view`. The row described all four surfaces as
/// agreed-and-wrong at two bodies and read that agreement as both backends
/// declining for one reason. It held with the trailing `let m = r;` and nowhere
/// else: `norebind` (the same shape without it) measured ONE body interpreted
/// against TWO on all three compiled surfaces, and `method` did the same. The
/// interpreter had been retracting its own slots for an inherited root all along —
/// only its PROPAGATION onto the bound names was withheld — so the withholding was
/// not holding the backends together, which was its whole justification. A live
/// run-vs-build split sat inside a row filed as agreed because one spelling was
/// measured and the neighbouring one was not.
///
/// `local` and `noproj` are the over-reach controls, failing in opposite
/// directions: a local tuple source owns its element and must keep its single
/// body, and a rebind with no projection at all must not lose the caller's.
///
/// THE LOCAL PROJECTION CONTROL IS DELIBERATELY ABSENT. `let h = H { … };
/// let h2 = h; let (r, k) = h2.pe;` prints the body BEFORE the binding is read on
/// all three compiled surfaces — B-2026-09-02-43, an open row about a cap-zeroed
/// husk, unrelated to view-ness and untouched here. Pinning it would make this
/// twin fail when that row is fixed, and would claim a cell this change does not
/// own.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_inherited_param_view_root_destructure_is_a_view`, pinned to the same string.
#[test]
fn test_inherited_param_view_root_destructure_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H    { pe: (R, i64) }
struct Hold { n: i64 }

fn g1(h: H)         { let h2 = h; let (r, k) = h2.pe; println(f"  b{r.id}") }
fn g2(h: H)         { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") }
fn g3(h: H)         { let h2 = h; let h3 = h2; let (r, k) = h3.pe; let m = r; println(f"  b{m.id}") }
fn g4(t: (R, i64))  { let t2 = t; let (r, k) = t2; let m = r; println(f"  b{m.id}") }
impl Hold { fn take(ref self, h: H) { let h2 = h; let (r, k) = h2.pe; let m = r; println(f"  b{m.id}") } }
fn g6()             { let t = (R { id: 26 }, 0); let t2 = t; let (r, k) = t2; let m = r; println(f"  b{m.id}") }
fn g7(h: H)         { let h2 = h; println("  np") }

fn main() {
    println("norebind"); g1(H { pe: (R { id: 21 }, 0) });                     println("norebind end")
    println("rebind");   g2(H { pe: (R { id: 22 }, 0) });                     println("rebind end")
    println("twohop");   g3(H { pe: (R { id: 23 }, 0) });                     println("twohop end")
    println("tupreb");   g4((R { id: 24 }, 0));                               println("tupreb end")
    println("method");   let o = Hold { n: 0 }; o.take(H { pe: (R { id: 25 }, 0) }); println("method end")
    println("local");    g6();                                                println("local end")
    println("noproj");   g7(H { pe: (R { id: 27 }, 0) });                     println("noproj end")
    println("done")
}
"#),
        r#"norebind
  b21
dR21
norebind end
rebind
  b22
dR22
rebind end
twohop
  b23
dR23
twohop end
tupreb
  b24
dR24
tupreb end
method
  b25
dR25
method end
local
  b26
dR26
local end
noproj
  np
dR27
noproj end
done
"#
    );
}

/// B-2026-09-02-43 — a `let`-destructure over a LOCAL struct's tuple field
/// (`let h = H {{ pe: (mk(21), 0) }}; let (r, k) = h.pe;`) hands each element's
/// `Drop` body to its leaf, so the owner's walk must stop running it.
///
/// It did not. The owner's walk fired the same body a SECOND time, against the slot
/// `zero_tuple_elem_cap_at` had just emptied, and fired it FIRST — before the live
/// read. The user body therefore observed a value whose `String` read empty and
/// whose `Vec` read length zero: `dR21//0`, then `b21/t21/1`, then the real
/// `dR21/t21/1`, on all three compiled surfaces against one live body under
/// `--interp`.
///
/// WHY EVERY EXISTING TEST IN THIS FAMILY MISSES IT, and why this one renders. The
/// husk is ZEROED, not freed, so nothing double-frees: valgrind and LSan are clean
/// at `-O0` and `-O2` alike, and the memory-sanitizer suite cannot see it. The
/// regression tests around it assert body COUNTS over an `R {{ id: i64 }}` — a
/// struct with nothing that can read empty — so a body running on a husk is
/// indistinguishable from one running on the live value. `R` here carries a
/// `String` and a `Vec` and the body prints both, which is the only instrument that
/// detects this class at all. A count-only assertion would have passed on the
/// defect before the fix, because the COUNT was also wrong but in a way the
/// `--interp` comparison already covered.
///
/// The repair is the mask this arm never consulted. `skip.nested[i].here` means
/// "indices masked inside field i's own walker" — inner FIELD indices for a struct
/// field, ELEMENT indices for a tuple one — and the struct recursion honoured it
/// while the tuple arm called the unmasked emitter.
///
/// `baretuple` and `paramroot` are the over-reach controls on the two legs this
/// does not touch: a bare tuple LOCAL was always correct (no field hop), and a
/// PARAM root takes the other leg of `owner_runs_bodies`, where the source keeps
/// the body and the leaf must not gain one. `sibling` proves the mask is scoped to
/// the destructured field — `other` still runs, on a live value — and `bothelems`
/// that a tuple whose elements are BOTH taken masks both. `noDestr` reads the
/// element without destructuring at all and must keep the owner's single body.
///
/// WIDENED TO ANY DEPTH by B-2026-09-03-11. The one-hop restriction this test
/// originally carried was a KEYING limit on both sides, not an ownership judgement:
/// codegen's `struct_moved_nested_field_bodies` was keyed outer-field -> inner
/// indices, and the interpreter's flat mask keys `(name, field)` and its writer
/// only fired when the projected object was an IDENTIFIER. So `let (r, k) = g.h.pe`
/// recorded nothing anywhere, and BOTH backends doubled -- which is why it could
/// not be folded in here and had to move with the interpreter's own path-keyed
/// mask. `twohop` and `threehop` pin the widening; `outersib` and `midsib` pin that
/// it stays scoped, a `Drop` sibling at the OUTER and at the INTERMEDIATE level
/// each keeping its body; `deepNoDestr` reads through the whole chain without
/// destructuring and must keep the owner's single body.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_local_struct_tuple_field_destructure_masks_the_owner`, pinned to the same string.
#[test]
fn test_local_struct_tuple_field_destructure_masks_the_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H  { pe: (R, i64) }
struct H2 { pe: (R, i64), other: R }
struct H3 { pe: (R, R) }
struct G  { h: H }
struct G2 { h: H, outer: R }
struct G3 { h: H2 }
struct G4 { g: G }

fn n1() { let h = H { pe: (mk(21), 0) }; let (r, k) = h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n2() { let h = H { pe: (mk(22), 0) }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n3() { let t = (mk(23), 0); let (r, k) = t; let m = r; println(f"  b{m.id}/{m.tag}") }
fn n4() { let h = H2 { pe: (mk(24), 0), other: mk(94) }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n5() { let h = H3 { pe: (mk(25), mk(95)) }; let (a, b) = h.pe; println(f"  b{a.id}/{b.id}") }
fn n6() { let h = H { pe: (mk(26), 0) }; println(f"  b{h.pe.0.id}/{h.pe.0.tag}") }
fn n7(h: H) { let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}") }
fn n8() { let g = G { h: H { pe: (mk(31), 0) } }; let (r, k) = g.h.pe; let m = r; println(f"  b{m.id}/{m.tag}/{m.xs.len()}") }
fn n9() { let g = G2 { h: H { pe: (mk(32), 0) }, outer: mk(82) }; let (r, k) = g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n10() { let g = G3 { h: H2 { pe: (mk(33), 0), other: mk(83) } }; let (r, k) = g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n11() { let g = G4 { g: G { h: H { pe: (mk(34), 0) } } }; let (r, k) = g.g.h.pe; println(f"  b{r.id}/{r.tag}") }
fn n12() { let g = G { h: H { pe: (mk(35), 0) } }; println(f"  b{g.h.pe.0.id}/{g.h.pe.0.tag}") }

fn main() {
    println("local1hop");   n1();                      println("local1hop end")
    println("norebind");    n2();                      println("norebind end")
    println("baretuple");   n3();                      println("baretuple end")
    println("sibling");     n4();                      println("sibling end")
    println("bothelems");   n5();                      println("bothelems end")
    println("noDestr");     n6();                      println("noDestr end")
    println("paramroot");   n7(H { pe: (mk(27), 0) }); println("paramroot end")
    println("twohop");      n8();                      println("twohop end")
    println("outersib");    n9();                      println("outersib end")
    println("midsib");      n10();                     println("midsib end")
    println("threehop");    n11();                     println("threehop end")
    println("deepNoDestr"); n12();                     println("deepNoDestr end")
    println("done")
}
"#),
        r#"local1hop
  b21/t21/1
dR21/t21/1
local1hop end
norebind
  b22/t22
dR22/t22/1
norebind end
baretuple
  b23/t23
dR23/t23/1
baretuple end
sibling
dR94/t94/1
  b24/t24
dR24/t24/1
sibling end
bothelems
  b25/95
dR95/t95/1
dR25/t25/1
bothelems end
noDestr
  b26/t26
dR26/t26/1
noDestr end
paramroot
  b27/t27
dR27/t27/1
paramroot end
twohop
  b31/t31/1
dR31/t31/1
twohop end
outersib
dR82/t82/1
  b32/t32
dR32/t32/1
outersib end
midsib
dR83/t83/1
  b33/t33
dR33/t33/1
midsib end
threehop
  b34/t34
dR34/t34/1
threehop end
deepNoDestr
  b35/t35
dR35/t35/1
deepNoDestr end
done
"#
    );
}

/// B-2026-09-03-12 — a tuple bound out of a PLACE (`let x = h.pe;`) records its
/// element types, so the binding runs the element's `Drop` body and can be
/// projected.
///
/// TWO FAILURES FROM ONE MISSING RECORD, and the row was filed for the smaller.
/// `x.0.id` failed `karac build` with the loud "cannot resolve field ... its type
/// was not recorded for codegen" — a run-vs-build divergence, but one that stops
/// the build. The same absent record ALSO cost the element its body outright:
/// `bound` ran ONE body under `--interp` and ZERO on all three compiled surfaces, a
/// user `Drop` that never fires and says nothing. That half was found by
/// re-measuring the row's own repro with a body that RENDERS, the instrument
/// B-2026-09-02-43 established for this family, and it is why the cells here print
/// a `String` and a `Vec.len()` rather than counting.
///
/// THE NAIVE FIX DOUBLE-FREES, which is what the `bound` cell really guards.
/// Recording the element `TypeExpr`s alone gives this binding a bodies walker while
/// the owning struct's `NestedTuple` drop still frees the same buffers — an
/// immediate `free(): double free detected in tcache 2`. The binding has to take
/// the MEMORY with the bodies, exactly as the DESTRUCTURE spelling of the identical
/// source already does per element. So a future change that keeps the record and
/// drops the cap-zeroing passes a body-count assertion and aborts at runtime; this
/// cell fails instead.
///
/// TWO REGISTRIES, not one, which is why `project` and `bound` are separate cells.
/// The full `TypeExpr`s drive the drop walk; a `TupleIndex` RECEIVER is typed from
/// the parallel per-element NAMES registry, whose `.or_else` chain is this exact
/// family of gaps filed one at a time — annotation, literal, whole rebind
/// (B-2026-09-02-39), call result (B-2026-08-28-3). The place source was the one
/// member never added.
///
/// `literal`, `nobind` and `paramroot` are the over-reach controls: a
/// literal-bound tuple and an unbound chain read both worked before and must not
/// move, and a PARAM root's element body belongs to the caller and must not gain a
/// second owner here. `destr` and `rebound` are the shapes that inherit from this
/// binding, and `deep` is the two-hop source.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_place_source_tuple_binding_records_its_elements`, pinned to the same string.
#[test]
fn test_place_source_tuple_binding_records_its_elements() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct H { pe: (R, i64) }
struct G { h: H }

fn u1()      { let h = H { pe: (mk(41), 0) }; let x = h.pe; println("  bound") }
fn u2()      { let h = H { pe: (mk(42), 0) }; let x = h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u3()      { let h = H { pe: (mk(43), 0) }; let x = h.pe; let (a, b) = x; println(f"  b{a.id}/{a.tag}") }
fn u4()      { let g = G { h: H { pe: (mk(44), 0) } }; let x = g.h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u5()      { let t = (mk(45), 0); let x = t; println(f"  b{x.0.id}/{x.0.tag}") }
fn u6()      { let h = H { pe: (mk(46), 0) }; println(f"  b{h.pe.0.id}/{h.pe.0.tag}") }
fn u7(h: H)  { let x = h.pe; println(f"  b{x.0.id}/{x.0.tag}") }
fn u8()      { let h = H { pe: (mk(48), 0) }; let x = h.pe; let y = x; println(f"  b{y.0.id}/{y.0.tag}") }

fn main() {
    println("bound");     u1();                      println("bound end")
    println("project");   u2();                      println("project end")
    println("destr");     u3();                      println("destr end")
    println("deep");      u4();                      println("deep end")
    println("literal");   u5();                      println("literal end")
    println("nobind");    u6();                      println("nobind end")
    println("paramroot"); u7(H { pe: (mk(47), 0) }); println("paramroot end")
    println("rebound");   u8();                      println("rebound end")
    println("done")
}
"#),
        r#"bound
dR41/t41/1
  bound
bound end
project
  b42/t42
dR42/t42/1
project end
destr
  b43/t43
dR43/t43/1
destr end
deep
  b44/t44
dR44/t44/1
deep end
literal
  b45/t45
dR45/t45/1
literal end
nobind
  b46/t46
dR46/t46/1
nobind end
paramroot
  b47/t47
dR47/t47/1
paramroot end
rebound
  b48/t48
dR48/t48/1
rebound end
done
"#
    );
}

/// B-2026-09-02-39 — A TUPLE PARAM'S ELEMENT TYPES WERE NEVER RECORDED, so
/// any element a NAME cannot spell was invisible to codegen.
///
/// `tuple_var_elem_type_exprs` — the registry `place_chain_tuple_tes` prefers
/// and every tuple-element consumer reads through — was populated from
/// exactly ONE place: a tuple ANNOTATION on a `let`. A by-value tuple PARAM
/// has a full declared type and registered nothing; a whole rebind and a
/// destructure leaf inherited nothing. The names-derived fallback covers a
/// FLAT element by name and renders anything a name cannot spell — a NESTED
/// tuple above all — as an EMPTY `Path`, which every consumer reads as "no
/// such leaf" and skips.
///
/// `param` / `rebind` / `leaf` all FAILED TO LOWER before this fix, with
/// `cannot resolve field 'id' on this receiver (its type was not recorded for
/// codegen)`, while `--interp` printed every one of them.
///
/// THE CONTROLS ARE WHAT LOCALIZED THE FAULT, and they are the reason the fix
/// is a registration rather than anything downstream:
/// - `flat` — `t.0.id` on a flat tuple param. Always worked; the NAME
///   spelling suffices for a single-segment path.
/// - `annotated` — the identical nested read off an ANNOTATED LOCAL. Already
///   lowered and printed BEFORE the fix, which proves the whole consumer
///   chain handles a nested element correctly once the `TypeExpr`s are
///   present. An annotation is therefore also a working user-side workaround.
///
/// `reuse_param` / `reuse_local` are the hygiene half, and they are not
/// decoration. `tuple_var_elem_tes()` prefers this registry WHOLESALE, so a
/// stale entry does not merely add detail — it WINS over the next function's
/// correct names-derived spelling. Nothing cleared the registry per function;
/// the leak predates this row (the annotated-`let` site has always written
/// there), but registering every tuple param turns a shape you had to
/// construct on purpose into one any two functions sharing a param name would
/// hit. These two cells reuse the name `t` at a DIFFERENT tuple type and must
/// both print their own.
///
/// THIS BACKEND WAS RIGHT THROUGHOUT — every cell below printed correctly
/// before the fix. It is pinned as the ORACLE the compiled backends converged
/// to, so a future change that moves this side is caught as loudly as one that
/// moves theirs.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_tuple_param_element_types_are_recorded`, pinned to the same string.
#[test]
fn test_tuple_param_element_types_are_recorded() {
    assert_eq!(
        run(r#"struct R { id: i64 }
struct Z { tag: String }

// FAILING TODAY
fn q1(t: ((R, i64), i64)) { println(f"q1 {t.0.0.id}") }
fn q2(t: (R, i64))        { let t2 = t; println(f"q2 {t2.0.id}") }
fn q3(t: ((R, i64), i64)) { let (inner, y) = t; println(f"q3 {inner.0.id}") }
// CONTROLS THAT WORK
fn q4(t: (R, i64))        { println(f"q4 {t.0.id}") }
fn q5() { let t: ((R, i64), i64) = ((R { id: 5 }, 0), 0); println(f"q5 {t.0.0.id}") }
// CROSS-FUNCTION NAME REUSE: `t` is a DIFFERENT tuple type here
fn q6(t: (Z, i64))        { println(f"q6 {t.0.tag}") }
fn q7() { let t = (Z { tag: "z7" }, 0); println(f"q7 {t.0.tag}") }

fn main() {
    q1(((R { id: 1 }, 0), 0))
    q2((R { id: 2 }, 0))
    q3(((R { id: 3 }, 0), 0))
    q4((R { id: 4 }, 0))
    q5()
    q6((Z { tag: "z6" }, 0))
    q7()
}
"#),
        r#"q1 1
q2 2
q3 3
q4 4
q5 5
q6 z6
q7 z7
"#
    );
}

/// B-2026-08-01-12 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_param_struct_destructure_single_caller_fire`, same source and
/// expected string. Pre-fix the interpreter registered Drop slots for the
/// fields a struct destructure of an OWNED param bound
/// (`let Holder { r } = h;`), so the body fired a second time inside the
/// callee (after "got N", before "take done") on top of the caller-side
/// fire codegen and the interpreter share; the destructure gate
/// (`let_destructures_owned_param` + `owned_param_names_stack`) now binds
/// views, leaving exactly the caller's single fire.
#[test]
fn test_param_struct_destructure_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             fn take(h: Holder) {\n\
                 let Holder { r } = h;\n\
                 println(f\"got {r.id}\");\n\
                 println(\"take done\");\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
                 take(x);\n\
                 println(\"b\");\n\
                 take(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ngot 5\ntake done\ndrop 5 y5\nb\ngot 7\ntake done\ndrop 7 y7\nend\n"
    );
}

/// B-2026-08-27-48 — a struct DESTRUCTURED out of an owned TUPLE param
/// (`let (r, n) = p;`) fires its user `Drop` body ONCE, not twice. Pre-fix
/// the interpreter registered Drop slots for the bindings the tuple pattern
/// introduced, so the element's body ran at the callee block's cleanup ON TOP
/// of the caller's fire, while both compiled backends ran only the caller's:
/// `drop 41` twice under `karac run --interp`, once under `karac build`.
/// `PatternKind::Tuple` now joins `Struct`/`TupleVariant` on the
/// `let_destructures_owned_param` gate, so the bindings are param views.
///
/// Three argument shapes in one program, because they reach the caller's fire
/// by different routes and the first fix attempt got two of them wrong:
/// a fresh tuple LITERAL (the row's repro, caller fires via
/// `run_fresh_temp_arg_drops`), a PLACE argument under a distinct name
/// (`q`, caller fires via its own tuple element walk), and a place argument
/// whose name COLLIDES with the callee's param (`p`). The collision case is
/// the reason `moved_out_container_bodies_bindings` and its two element
/// siblings are now frame-isolated in `eval_call`: name-keyed and unscoped,
/// the callee's own move disarmed the caller's identically-spelled binding,
/// which cancelled this very double-fire and made the shape read as correct.
/// Twin of `tests/codegen.rs`'s
/// `e2e_tuple_param_destructure_single_caller_fire`, same source and string.
#[test]
fn test_tuple_param_destructure_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             fn take(p: (Res, i64)) {\n\
             \x20   let (r, n) = p;\n\
             \x20   println(f\"got {r.id} {n}\");\n\
             \x20   println(\"take done\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   take((Res { id: 5, name: f\"y{5}\" }, 1));\n\
             \x20   println(\"b\");\n\
             \x20   let q = (Res { id: 7, name: f\"y{7}\" }, 2);\n\
             \x20   take(q);\n\
             \x20   println(\"c\");\n\
             \x20   let p = (Res { id: 9, name: f\"y{9}\" }, 3);\n\
             \x20   take(p);\n\
             \x20   println(\"end\");\n\
             }\n"),
        "a\ngot 5 1\ntake done\ndrop 5 y5\nb\ngot 7 2\ntake done\ndrop 7 y7\nc\ngot 9 3\ntake done\ndrop 9 y9\nend\n"
    );
}

/// B-2026-08-28-23 — a NESTED projection out of an owned struct param runs the
/// escaping field's body once. Interpreter twin of `tests/codegen.rs`'s
/// `e2e_nested_projection_runs_the_returned_fields_body_once`, whose doc carries
/// the reasoning.
///
/// `sibling` is the row that forbids masking the one-level PREFIX instead of the
/// path: `s` really does die inside the call, and a prefix mask would take its
/// only body away — a false escape, the direction this analysis is built to
/// avoid.
#[test]
fn test_nested_projection_runs_the_returned_fields_body_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct I { r: R }\n\
         struct W { inner: I, n: i64 }\n";
    for (label, body, want) in [
        (
            "nested-projection",
            "fn take(w: W) -> R { w.inner.r }\n\
             fn main() { let x = take(W { inner: I { r: R { id: 41 } }, n: 1 });\n\
             \x20           println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "sibling",
            "struct I2 { r: R, s: R }\n\
             struct W2 { inner: I2, n: i64 }\n\
             fn take(w: W2) -> R { w.inner.r }\n\
             fn main() { let x = take(W2 { inner: I2 { r: R { id: 42 }, s: R { id: 52 } },\n\
             \x20                         n: 2 });\n\
             \x20           println(f\"{x.id}\") }\n",
            "drop 52\n42\ndrop 42\n",
        ),
        (
            "nested-destructure",
            "fn take(w: W) -> R { let W { inner, n } = w; let I { r } = inner; r }\n\
             fn main() { let x = take(W { inner: I { r: R { id: 43 } }, n: 3 });\n\
             \x20           println(f\"{x.id}\") }\n",
            "43\ndrop 43\n",
        ),
        (
            "own-drop-mid",
            "struct Id { r: R }\n\
             impl Drop for Id { fn drop(mut ref self) { println(\"drop Id\") } }\n\
             struct Wd { inner: Id, n: i64 }\n\
             fn take(w: Wd) -> R { w.inner.r }\n\
             fn main() { let x = take(Wd { inner: Id { r: R { id: 44 } }, n: 4 });\n\
             \x20           println(f\"{x.id}\") }\n",
            "drop Id\n44\ndrop 44\n",
        ),
        // CONTROL — a ONE-level projection, the answer the widening had to
        // leave untouched while adding the deeper one.
        (
            "whole-inner",
            "fn take(w: W) -> I { w.inner }\n\
             fn main() { let x = take(W { inner: I { r: R { id: 45 } }, n: 5 });\n\
             \x20           println(f\"{x.r.id}\") }\n",
            "45\ndrop 45\n",
        ),
        // CONTROL — nothing escapes.
        (
            "nothing-escapes-control",
            "fn take(w: W) -> i64 { w.n }\n\
             fn main() { let g = take(W { inner: I { r: R { id: 46 } }, n: 6 });\n\
             \x20           println(f\"{g}\") }\n",
            "drop 46\n6\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// A tuple param returned WHOLE, bound to a local, then destructured at the call
/// site runs its element's user `Drop` body (B-2026-08-28-18).
///
/// This backend was the CORRECT one — it printed the body while both compiled
/// backends printed nothing — so this fixture is the parity anchor rather than
/// the regression proof. Keeping it is what makes the twin in `tests/codegen.rs`
/// (`e2e_passthrough_tuple_destructured_at_the_call_site_runs_its_body`) an
/// assertion about agreement rather than about one backend's opinion.
#[test]
fn test_passthrough_tuple_destructured_at_the_call_site_runs_its_body() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        (
            "passthrough",
            "fn take(p: (R, i64)) -> (R, i64) { p }\n\
             fn main() { let x = take((R { id: 41 }, 1)); let (r, n) = x;\n\
             \x20           println(f\"{r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "passthrough-no-local",
            "fn take(p: (R, i64)) -> (R, i64) { p }\n\
             fn main() { let (r, n) = take((R { id: 42 }, 1)); println(f\"{r.id}\") }\n",
            "42\ndrop 42\n",
        ),
        // CONTROL — the callee BUILDS the tuple rather than passing one through.
        (
            "builds-its-own",
            "fn make() -> (R, i64) { return (R { id: 43 }, 1); }\n\
             fn main() { let y = make(); let (a, b) = y; println(f\"{a.id}\") }\n",
            "43\ndrop 43\n",
        ),
        // CONTROL — the STRUCT spelling of the passthrough.
        (
            "struct-passthrough",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> W { w }\n\
             fn main() { let x = take(W { r: R { id: 44 }, n: 1 }); let W { r, n } = x;\n\
             \x20           println(f\"{r.id}\") }\n",
            "44\ndrop 44\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-30-54 — the FIELD spelling of the conditional param-view
/// assignment, `h.f = r`.
///
/// Unlike its sibling below, this half was NOT an oracle before the fix: the
/// interpreter ran the payload's body TWICE on the taken path
/// (`dR0 m dR8 dE dR8 b8`) because `record_assign_of_param_view` reached only
/// a bare-identifier target, and codegen was wrong in the other direction. The
/// fix gives the interpreter a per-FIELD record over
/// `moved_out_struct_field_bodies` — the mask
/// `drop_user_drop_fields_of_binding` already consults — rather than the
/// whole-binding set the identifier leg uses, so silencing one field leaves
/// every other field's body armed.
///
/// The parity twin of
/// `codegen::test_e2e_field_target_param_view_assign_matches_the_identifier_spelling`,
/// and the expectations are the identifier test's own rows with `out`
/// replaced by `h.f` — that the two spellings print the same thing is the
/// property the row was filed to restore.
#[test]
fn field_target_param_view_assign_fires_each_body_once() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct H { f: R }\n\
         struct H2 { f: R, g: R }\n\
         fn dies(b: E) -> i64 {\n\
             let mut h: H = H { f: R { id: 0, tag: f\"t0\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             println(\"m\");\n\
             return h.f.id\n\
         }\n\
         fn refreshed(b: E) -> i64 {\n\
             let mut h: H = H { f: R { id: 0, tag: f\"t0\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             println(\"m1\");\n\
             h.f = R { id: 5, tag: f\"t5\" };\n\
             println(\"m2\");\n\
             return h.f.id\n\
         }\n\
         fn plain() -> i64 {\n\
             let mut h: H = H { f: R { id: 20, tag: f\"t20\" } };\n\
             h.f = R { id: 21, tag: f\"t21\" };\n\
             println(\"p\");\n\
             return h.f.id\n\
         }\n\
         fn sibs(b: E) -> i64 {\n\
             let mut h: H2 = H2 { f: R { id: 30, tag: f\"t30\" }, g: R { id: 31, tag: f\"t31\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             println(\"s\");\n\
             return h.f.id + h.g.id\n\
         }\n";
    for (label, body, want) in [
        // Arm NOT taken: `h.f` still holds what `h` was built with.
        (
            "arm-not-taken",
            "println(f\"a{dies(E.B)}\")\n",
            "m\ndR0\ndE\na0\n",
        ),
        // Arm taken: the displacement fires the field's own initializer at the
        // assignment, and the moved-in payload's body is the caller's. The
        // interpreter used to run that payload here as well.
        (
            "arm-taken",
            "println(f\"b{dies(E.A(R { id: 8, tag: f\"t8\" }))}\")\n",
            "dR0\nm\ndE\ndR8\nb8\n",
        ),
        // A view, then a FRESH value in the same field. The second
        // displacement must NOT fire (it displaces the caller's view) and the
        // fresh value must run its own body — the pair codegen needed a re-arm
        // for and the interpreter needed the per-field gate for.
        (
            "view-then-fresh-value",
            "println(f\"c{refreshed(E.A(R { id: 9, tag: f\"t9\" }))}\")\n",
            "dR0\nm1\nm2\ndR5\ndE\ndR9\nc5\n",
        ),
        // No view anywhere: the boundary an over-eager fix would move.
        (
            "no-view-at-all",
            "println(f\"d{plain()}\")\n",
            "dR20\np\ndR21\nd21\n",
        ),
        // TWO Drop-bearing fields with the arm not taken: both bodies are due.
        (
            "two-fields-not-taken",
            "println(f\"e{sibs(E.B)}\")\n",
            "s\ndR31\ndR30\ndE\ne61\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-09-02-10 — the ORACLE half of the per-FIELD param-view question: a
/// view landing in ONE field leaves every SIBLING field's body armed.
///
/// The interpreter has been right about this throughout, and that is the point
/// of pinning it here. It records the view through
/// `moved_out_struct_field_bodies`, a `(binding, field)` mask, so the
/// granularity of its answer has always matched the granularity of the reason.
/// Codegen's per-path bit was keyed by BINDING and guarded the base's whole
/// `UserDrop{StructFieldBodies}` action, so a view in `f` silenced `g` as well
/// — B-2026-08-01-19's documented over-suppression trade, which B-2026-08-30-54
/// narrowed and thereby made visible as a divergence for the first time.
///
/// Sibling of `field_target_param_view_assign_runs_each_body_once` above, whose
/// `two-fields-not-taken` row deliberately uses the arm that does NOT run:
/// these are the taken-arm rows it could not assert while codegen was coarse.
///
/// `two-views-one-taken` is the row a per-BINDING flag cannot satisfy however
/// it is repaired, and it is here to keep any future cut honest. Two fields
/// take views on INDEPENDENT paths; where only the first landed, one bool
/// cannot also say the second still owns its value. It needs one flag per
/// field, which is what the fix emits.
///
/// The parity twin of
/// `codegen::test_e2e_field_param_view_leaves_sibling_field_bodies_armed`.
#[test]
fn field_param_view_leaves_sibling_field_bodies_armed() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct H2 { f: R, g: R }\n\
         struct H3 { f: R, g: R, k: R }\n\
         fn sibs(b: E) -> i64 {\n\
             let mut h: H2 = H2 { f: R { id: 30, tag: f\"t30\" }, g: R { id: 31, tag: f\"t31\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             println(\"s\");\n\
             return h.f.id + h.g.id\n\
         }\n\
         fn sib_refreshed(b: E) -> i64 {\n\
             let mut h: H2 = H2 { f: R { id: 30, tag: f\"t30\" }, g: R { id: 31, tag: f\"t31\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             h.g = R { id: 9, tag: f\"t9\" };\n\
             println(\"s\");\n\
             return h.f.id + h.g.id\n\
         }\n\
         fn two_views(b: E, c: E) -> i64 {\n\
             let mut h: H3 = H3 { f: R { id: 30, tag: f\"t30\" }, g: R { id: 31, tag: f\"t31\" }, k: R { id: 32, tag: f\"t32\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             match c { E.A(r2) => { h.g = r2; } E.B => { } }\n\
             println(\"s\");\n\
             return h.f.id + h.g.id + h.k.id\n\
         }\n\
         fn rearm_with_sibling_viewed(b: E, c: E) -> i64 {\n\
             let mut h: H3 = H3 { f: R { id: 40, tag: f\"t40\" }, g: R { id: 41, tag: f\"t41\" }, k: R { id: 42, tag: f\"t42\" } };\n\
             match b { E.A(r) => { h.f = r; } E.B => { } }\n\
             match c { E.A(r2) => { h.g = r2; } E.B => { } }\n\
             h.f = R { id: 7, tag: f\"t7\" };\n\
             println(\"s\");\n\
             return h.f.id + h.g.id + h.k.id\n\
         }\n";
    for (label, body, want) in [
        // The row's own repro. `g` is never moved, never viewed, and read on
        // the line before; codegen printed this without `dR31`.
        (
            "sibling-armed-on-taken-arm",
            "println(f\"a{sibs(E.A(R { id: 8, tag: f\"t8\" }))}\")\n",
            "dR30\ns\ndR31\ndE\ndR8\na39\n",
        ),
        // The same loss one step later: a FRESH value in the sibling AFTER the
        // view. Note `dR31` here fires at the assignment (the displacement),
        // which codegen already had right — what it lost was `dR9`, the fresh
        // value's own body at the base's death.
        (
            "sibling-refreshed-after-view",
            "println(f\"b{sib_refreshed(E.A(R { id: 8, tag: f\"t8\" }))}\")\n",
            "dR30\ndR31\ns\ndR9\ndE\ndR8\nb17\n",
        ),
        // TWO fields viewed on independent paths, only the FIRST landing.
        // Codegen lost BOTH `dR32` and `dR31` here — the row that rules out
        // any single-bool repair.
        (
            "two-views-one-taken",
            "println(f\"c{two_views(E.A(R { id: 18, tag: f\"t18\" }), E.B)}\")\n",
            "dR30\ns\ndR32\ndR31\ndE\ndE\ndR18\nc81\n",
        ),
        // Neither view lands: the all-armed path, which must be untouched.
        (
            "no-view-lands",
            "println(f\"d{two_views(E.B, E.B)}\")\n",
            "s\ndR32\ndR31\ndR30\ndE\ndE\nd93\n",
        ),
        // A field re-armed by a fresh value while a SIBLING still holds a
        // view. Codegen could not express this at all before: its re-arm was
        // restricted to the case where the refreshed field was the only one
        // recorded, precisely because one bool could not hold both answers.
        (
            "rearm-while-sibling-viewed",
            "println(f\"e{rearm_with_sibling_viewed(E.B, E.A(R { id: 6, tag: f\"t6\" }))}\")\n",
            "dR41\ndR40\ns\ndR42\ndR7\ndE\ndR6\ndE\ne55\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-27-48, method leg — the same destructure inside an IMPL METHOD
/// fires ONCE. This is the guard on the gate's other edge: a method frame's
/// arguments get no caller-side fire (`run_fresh_temp_arg_drops` is wired
/// into `eval_call` alone), so retracting the callee's slot there removes the
/// ONLY fire. An intermediate version of this fix did exactly that and ran
/// zero bodies for `tup` against one on both compiled backends.
///
/// The `strc` half pins a repair this fix carries with it: the STRUCT
/// destructure has had that hole since B-2026-08-01-12 landed the free-fn
/// gate without the method distinction, so `let Holder { r } = h;` inside a
/// method fired zero bodies while both compiled backends fired one. Twin of
/// `tests/codegen.rs`'s `e2e_method_param_destructure_single_caller_fire`.
#[test]
fn test_method_param_destructure_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct W { v: i64 }\n\
             impl W {\n\
             \x20   fn tup(ref self, p: (Res, i64)) {\n\
             \x20       let (r, n) = p;\n\
             \x20       println(f\"tup {r.id} {n} {self.v}\");\n\
             \x20   }\n\
             \x20   fn strc(ref self, h: Holder) {\n\
             \x20       let Holder { r } = h;\n\
             \x20       println(f\"strc {r.id} {self.v}\");\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let w = W { v: 100 };\n\
             \x20   println(\"a\");\n\
             \x20   w.tup((Res { id: 5, name: f\"y{5}\" }, 1));\n\
             \x20   println(\"b\");\n\
             \x20   w.strc(Holder { r: Res { id: 7, name: f\"y{7}\" } });\n\
             \x20   println(\"end\");\n\
             }\n"),
        "a\ntup 5 1 100\ndrop 5 y5\nb\nstrc 7 100\ndrop 7 y7\nend\n"
    );
}

/// B-2026-09-15-28 — a `Vec[T]`-typed struct FIELD assigned a fresh container
/// runs the DISPLACED elements' `Drop` bodies, not only the surviving ones.
///
/// `h.v = [..]` ran the new generation's bodies at scope exit and none for the
/// generation it displaced, so this printed `dD3 dD4 mid end` where the
/// IDENTIFIER position (`v = [..]`, correct since B-2026-09-14-23) printed all
/// four. Both backends were silent here rather than divergent — the
/// interpreter's displaced-field `match` handled `Value::Struct` and
/// `Value::EnumVariant` and let a `Value::Array` fall through, while codegen's
/// `emit_displaced_field_bodies` gate keyed on the field's head type name and
/// so admitted only a struct or enum field. Both halves were fixed in one
/// commit: moving one alone converts an agreed gap into a run-vs-build
/// divergence, which is what B-2026-09-15-23 measured when a sibling was tried
/// codegen-first.
///
/// The assertion is the IDENTIFIER control's string verbatim — the whole point
/// is that the two positions now agree, ordering included.
#[test]
fn test_field_assign_runs_the_displaced_containers_element_bodies() {
    const SRC: &str = "struct D { id: i64, s: String }\n\
         impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
         fn mkd(n: i64) -> D { return D { id: n, s: f\"heap-{n}\" }; }\n";
    // The FIELD position — the row's own shape.
    assert_eq!(
        run(&format!(
            "{SRC}struct H {{ v: Vec[D] }}\n\
             fn main() {{\n\
             \x20   let mut h: H = H {{ v: [mkd(1), mkd(2)] }};\n\
             \x20   h.v = [mkd(3), mkd(4)];\n\
             \x20   println(\"mid\");\n\
             }}\n"
        )),
        "dD1\ndD2\ndD3\ndD4\nmid\n"
    );
    // The IDENTIFIER position, unchanged by this fix and the oracle for it.
    assert_eq!(
        run(&format!(
            "{SRC}fn main() {{\n\
             \x20   let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
             \x20   v = [mkd(3), mkd(4)];\n\
             \x20   println(\"mid\");\n\
             }}\n"
        )),
        "dD1\ndD2\ndD3\ndD4\nmid\n"
    );
}

/// B-2026-08-01-23 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_nested_container_elem_bodies`, same source and expected string.
#[test]
fn test_nested_container_elem_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut vv: Vec[Vec[Res]] = Vec.new();\n\
                 let mut inner: Vec[Res] = Vec.new();\n\
                 inner.push(Res { id: 7, name: f\"q{7}\" });\n\
                 vv.push(inner);\n\
                 println(\"b\");\n\
                 let mut m: Map[i64, Vec[Res]] = Map.new();\n\
                 let mut mi: Vec[Res] = Vec.new();\n\
                 mi.push(Res { id: 8, name: f\"r{8}\" });\n\
                 let _ = m.insert(1, mi);\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 7 q7\nb\ndrop 8 r8\nend\n"
    );
}

/// B-2026-08-01-20 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_field_assign_displaced_bodies`, same source and expected string.
#[test]
fn test_field_assign_displaced_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
                 o.h = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
                 println(f\"held {o.h.r.id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n"
    );
}

/// B-2026-08-01-30 leg A — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deep_chain_field_assign_displaced_bodies`, same source and expected
/// string (the interp leg is bodies-only; memory is GC'd). Pre-fix the
/// FieldAccess-target displaced fire was Identifier-base only, so a deep
/// chain (`o.h.r = <new>`) stayed silent.
#[test]
fn test_deep_chain_field_assign_displaced_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
                 o.h.r = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"held {o.h.r.id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n"
    );
}

/// B-2026-08-01-35 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_field_rooted_indexed_container_field_store`, same source and
/// expected string. The interp always applied these writes — this twin
/// pins the sequencing both backends must share now that codegen does
/// too.
#[test]
fn test_field_rooted_indexed_container_field_store() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Hi { r: Res }\n\
             struct Oi { hs: Vec[Hi] }\n\
             struct Ps { id: i64, name: String }\n\
             struct Os { hs: Vec[Ps] }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut o = Oi { hs: Vec.new() };\n\
                 o.hs.push(Hi { r: Res { id: 9, name: f\"z{9}\" } });\n\
                 o.hs[0].r = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"held {o.hs[0].r.id}\");\n\
                 let mut p = Os { hs: Vec.new() };\n\
                 p.hs.push(Ps { id: 9, name: f\"z{9}\" });\n\
                 p.hs[0].id = 4;\n\
                 let i = 0;\n\
                 p.hs[i].name = f\"w{6}\";\n\
                 println(f\"ps {p.hs[0].id} {p.hs[0].name}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\nheld 5\ndrop 5 y5\nps 4 w6\nend\n"
    );
}

/// B-2026-08-02-10 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_elem_method_receivers`, same source and expected string
/// (the interp ran these already — this pins the parity codegen now
/// shares for annotated tuple bindings).
#[test]
fn test_tuple_elem_method_receivers() {
    assert_eq!(
        run("fn main() {\n\
                 let v: Vec[i64] = Vec.new();\n\
                 let mut t: (Vec[i64], i64) = (v, 3);\n\
                 t.0.push(10);\n\
                 t.0.push(20);\n\
                 println(f\"len {t.0.len()} v0 {t.0[0]} v1 {t.0[1]}\");\n\
                 println(\"end\");\n\
             }\n"),
        "len 2 v0 10 v1 20\nend\n"
    );
}

#[test]
fn test_container_into_literal_field_arg_single_fire() {
    // B-2026-08-02-20 (leg 2) — interpreter twin of `tests/codegen.rs`'s
    // `e2e_container_into_literal_field_arg_single_fire`, same source and
    // expected string. Both backends double-fired pre-fix (parity-equal,
    // which is why no existing test caught it).
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Holder { xs: Vec[Res], tag: i64 }\n\
             fn main() {\n\
                 println(\"let-rhs:\");\n\
                 {\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 1, name: f\"a{1}\" });\n\
                     let h = Holder { xs: xs, tag: 3 };\n\
                     println(h.tag);\n\
                 }\n\
                 println(\"call-arg:\");\n\
                 {\n\
                     let mut v: Vec[Holder] = Vec.new();\n\
                     let mut ys: Vec[Res] = Vec.new();\n\
                     ys.push(Res { id: 2, name: f\"b{2}\" });\n\
                     v.push(Holder { xs: ys, tag: 4 });\n\
                     println(v.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "let-rhs:\n3\ndrop 1 a1\ncall-arg:\n1\ndrop 2 b2\nend\n"
    );
}

/// B-2026-08-01-19 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_param_field_store_single_caller_fire`, same source and expected
/// string. Pre-fix the base binding's Drop slot fired the caller-retained
/// value a second time at o's death; the FieldAccess-target branch in
/// `suppress_assign_move_user_drop` retracts it.
#[test]
fn test_param_field_store_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { r: Res }\n\
             struct Outer { h: Holder }\n\
             fn take(h: Holder) {\n\
                 let mut o = Outer { h: Holder { r: Res { id: 9, name: f\"z{9}\" } } };\n\
                 o.h = h;\n\
                 println(f\"held {o.h.r.id}\");\n\
                 println(\"take done\");\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let x = Holder { r: Res { id: 5, name: f\"y{5}\" } };\n\
                 take(x);\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ntake done\ndrop 5 y5\nend\n"
    );
}

/// B-2026-08-01-8 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_mixed_place_tuple_discard_single_intact_fire`, same source and
/// expected string. The interpreter's output text was already right (the
/// source binding's NLL walk fired intact); the fix moves the single fire
/// to the discard walk — the source's Drop action retracts at the
/// statement and the widened tuple gate admits the place element — so both
/// backends share one owner and one mechanism.
#[test]
fn test_mixed_place_tuple_discard_single_intact_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
                 println(\"a: all-fresh tuple discard\");\n\
                 let _ = (mk(1), 10);\n\
                 println(\"b: mixed fresh+place tuple discard\");\n\
                 let r = mk(2);\n\
                 let _ = (r, 20);\n\
                 println(\"c: place still-owned after\");\n\
                 let s = mk(3);\n\
                 let _ = (s.id, 30);\n\
                 println(\"end\");\n\
             }\n"),
        "a: all-fresh tuple discard\ndrop 1 r1\n\
         b: mixed fresh+place tuple discard\ndrop 2 r2\n\
         c: place still-owned after\ndrop 3 r3\nend\n"
    );
}

/// An unsigned value NESTED in a container or enum payload renders at its own
/// width (B-2026-08-19-27). The interpreter holds an unsigned value as its
/// two's-complement bit pattern in a signed carrier, so a `u64` at or above
/// 2^63 is a NEGATIVE `Value::Int`. The scalar `to_string` arm has always
/// consulted the receiver's type to read it back; the recursive renderer never
/// did — it walks `Value`s structurally — so every nested integer printed
/// signed and `println(o)` on an `Option[u64]` holding `u64::MAX` printed
/// `Some(-1)` while both compiled backends printed the value.
///
/// The renderer now takes the static type and peels one layer per level. Every
/// shape below diverged before the fix; the expected strings are exactly what
/// `karac build` produces, so this is an A/B assertion, not a guess.
#[test]
fn a_nested_unsigned_value_renders_at_its_own_width() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let big: u64 = 18446744073709551615u64;\n\
             let o: Option[u64] = Some(big);\n\
             println(o);\n\
             let r: Result[u64, String] = Ok(big);\n\
             println(r);\n\
             let mut v: Vec[u64] = vec![];\n\
             v.push(big);\n\
             v.push(5u64);\n\
             println(v);\n\
             let t = (big, 5i64);\n\
             println(t);\n\
             let mut vo: Vec[Option[u64]] = vec![];\n\
             vo.push(Some(big));\n\
             vo.push(None);\n\
             println(vo);\n\
             let mut mp: Map[String, u64] = Map.new();\n\
             mp.insert(\"k\", big);\n\
             println(mp);\n\
             println(f\"{o} {v}\");\n\
             }"
        ),
        "Some(18446744073709551615)\n\
         Ok(18446744073709551615)\n\
         [18446744073709551615, 5]\n\
         (18446744073709551615, 5)\n\
         [Some(18446744073709551615), None]\n\
         {k: 18446744073709551615}\n\
         Some(18446744073709551615) [18446744073709551615, 5]\n"
    );
}

/// B-2026-09-06-34 — this backend's half: `run_wildcard_destructure_leaf_user_drops`
/// now collects the fields a `..` rest leaves unnamed (and skips a param-view
/// field on either spelling).
///
/// Twin of `tests/codegen.rs`'s `e2e_struct_destructure_rest_fields_run_their_bodies_once`, pinned to the same string.
#[test]
fn test_struct_destructure_rest_fields_run_their_bodies_once() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}", xs: [i] }; }
struct S3 { a: R, b: R }
struct S4 { a: R, b: R, c: R, n: i64 }
fn mks(i: i64) -> S3 { return S3 { a: mk(i), b: mk(i + 1) }; }

fn local_a(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, .. } = s; println("  mid"); return a.id; }
fn local_ab(i: i64) -> i64 { let s = S4 { a: mk(i), b: mk(i + 1), c: mk(i + 2), n: 4 }; let S4 { a, b, .. } = s; println("  mid"); return a.id + b.id; }
fn local_all_rest(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { .. } = s; println("  mid"); return 1; }
fn local_wild(i: i64) -> i64 { let s = S3 { a: mk(i), b: mk(i + 1) }; let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn lit_a(i: i64) -> i64 { let S3 { a, .. } = S3 { a: mk(i), b: mk(i + 1) }; println("  mid"); return a.id; }
fn lit_wild(i: i64) -> i64 { let S3 { a, b: _ } = S3 { a: mk(i), b: mk(i + 1) }; println("  mid"); return a.id; }
fn call_a(i: i64) -> i64 { let S3 { a, .. } = mks(i); println("  mid"); return a.id; }
fn param_a(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return 1; }
fn view_w(r: R) -> i64 { let s = S3 { a: mk(92), b: r }; let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn view_a(r: R) -> i64 { let s = S3 { a: mk(90), b: r }; let S3 { a, .. } = s; println("  mid"); return a.id; }

fn main() {
    println("local_a"); let v1 = local_a(1); println(f"  v={v1}");
    println("local_ab"); let v2 = local_ab(10); println(f"  v={v2}");
    println("local_all_rest"); let v3 = local_all_rest(20); println(f"  v={v3}");
    println("local_wild"); let v4 = local_wild(30); println(f"  v={v4}");
    println("lit_a"); let v5 = lit_a(40); println(f"  v={v5}");
    println("lit_wild"); let v6 = lit_wild(50); println(f"  v={v6}");
    println("call_a"); let v7 = call_a(60); println(f"  v={v7}");
    println("param_a"); let v8 = param_a(S3 { a: mk(70), b: mk(71) }); println(f"  v={v8}");
    println("view_a"); let v9 = view_a(mk(80)); println(f"  v={v9}");
    println("view_w"); let v10 = view_w(mk(82)); println(f"  v={v10}");
    println("end");
}
"#),
        r#"local_a
  dR2
  mid
  dR1
  v=1
local_ab
  dR12
  mid
  dR11
  dR10
  v=21
local_all_rest
  dR21
  dR20
  mid
  v=1
local_wild
  dR31
  mid
  dR30
  v=30
lit_a
  dR41
  mid
  dR40
  v=40
lit_wild
  dR51
  mid
  dR50
  v=50
call_a
  dR61
  mid
  dR60
  v=60
param_a
  mid
  dR71
  dR70
  v=1
view_a
  mid
  dR90
  dR80
  v=90
view_w
  mid
  dR92
  dR82
  v=92
end
"#
    );
}

/// B-2026-09-06-41 — the backend that panicked. The fresh-temp argument
/// walk's two struct branches now filter escaping parts by a leaf that
/// carries a user `Drop`, as the tuple branch always did; a scalar read is
/// not a mask.
///
/// Twin of `tests/codegen.rs`'s `e2e_scalar_read_off_a_param_destructure_leaf_does_not_mask_the_walk`, pinned to the same string.
#[test]
fn test_scalar_read_off_a_param_destructure_leaf_does_not_mask_the_walk() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}", xs: [i] }; }
struct S3 { a: R, b: R }

fn full_ret(s: S3) -> i64 { let S3 { a, b } = s; println("  mid"); return b.id; }
fn full_let(s: S3) -> i64 { let S3 { a, b } = s; println("  mid"); let x = a.id + b.id; return x; }
fn rest_ret(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return a.id; }
fn wild_ret(s: S3) -> i64 { let S3 { a, b: _ } = s; println("  mid"); return a.id; }
fn rest_len(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); let n = a.name.len(); return n; }
fn rest_none(s: S3) -> i64 { let S3 { a, .. } = s; println("  mid"); return 1; }
fn whole_b(s: S3) -> R { let S3 { a, b } = s; println("  mid"); return b; }

fn main() {
    println("full_ret"); let v1 = full_ret(S3 { a: mk(1), b: mk(2) }); println(f"  v={v1}");
    println("full_let"); let v2 = full_let(S3 { a: mk(3), b: mk(4) }); println(f"  v={v2}");
    println("rest_ret"); let v3 = rest_ret(S3 { a: mk(5), b: mk(6) }); println(f"  v={v3}");
    println("wild_ret"); let v4 = wild_ret(S3 { a: mk(7), b: mk(8) }); println(f"  v={v4}");
    println("rest_len"); let v5 = rest_len(S3 { a: mk(9), b: mk(10) }); println(f"  v={v5}");
    println("rest_none"); let v6 = rest_none(S3 { a: mk(11), b: mk(12) }); println(f"  v={v6}");
    println("whole_b"); let r7 = whole_b(S3 { a: mk(13), b: mk(14) }); println(f"  v={r7.id}");
    println("named"); let s8 = S3 { a: mk(15), b: mk(16) }; let v8 = full_ret(s8); println(f"  v={v8}");
    println("end");
}
"#),
        r#"full_ret
  mid
  dR2
  dR1
  v=2
full_let
  mid
  dR4
  dR3
  v=7
rest_ret
  mid
  dR6
  dR5
  v=5
wild_ret
  mid
  dR8
  dR7
  v=7
rest_len
  mid
  dR10
  dR9
  v=2
rest_none
  mid
  dR12
  dR11
  v=1
whole_b
  mid
  dR13
  v=14
  dR14
named
  mid
  dR16
  dR15
  v=16
end
"#
    );
}

/// B-2026-09-06-45 — the interpreter half of the nested `self` rebind: the
/// caller's retained walk stands down for the whole call and this frame adopts
/// the receiver's body, running it on the paths where the rebind did not
/// happen. One path runs per call here, so the adoption needs no flag — the
/// rebind statement marks the receiver moved out and the frame's drop declines.
///
/// Twin of `tests/codegen.rs`'s `e2e_nested_self_rebind_runs_each_body_once`, pinned to the same string.
#[test]
fn test_nested_self_rebind_runs_each_body_once() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct S { r: R, n: i64 }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.n}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }

impl E {
    fn cond_match(self, c: bool) -> i64 {
        if c { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
        else { match self { E.A(r) => { return r.id + 100; } E.B => { return 100; } } }
    }
    fn cond_bare(self, c: bool) -> i64 { if c { let e = self; return 7; } return 0; }
    fn cond_mut(self, c: bool) -> i64 { if c { let mut e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } } else { return 5; } }
    fn cond_loop(self, n: i64) -> i64 { let mut i = 0; while i < n { let e = self; return 9; } return 0; }
    fn cond_arm(self, k: i64) -> i64 { match k { 1 => { let e = self; return 1; } _ => { return 2; } } }
    fn top_let(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn plain(self, c: bool) -> i64 { if c { return 1; } return 2; }
    fn borrowed(ref self, c: bool) -> i64 { if c { return 1; } return 2; }
}

impl S {
    fn cond_struct(self, c: bool) -> i64 { if c { let s2 = self; return s2.n; } return self.n + 100; }
    fn both_arms(self, c: bool) -> i64 { if c { let s1 = self; return s1.n; } else { let s2 = self; return s2.n + 50; } }
}

fn main() {
    println("enum_true"); let a = E.A(mk(1)); let v1 = a.cond_match(true); println(f"  v={v1}");
    println("enum_false"); let b = E.A(mk(2)); let v2 = b.cond_match(false); println(f"  v={v2}");
    println("bare_true"); let c = E.A(mk(3)); let v3 = c.cond_bare(true); println(f"  v={v3}");
    println("bare_false"); let d = E.A(mk(4)); let v4 = d.cond_bare(false); println(f"  v={v4}");
    println("mut_true"); let e = E.A(mk(5)); let v5 = e.cond_mut(true); println(f"  v={v5}");
    println("loop_once"); let f = E.A(mk(6)); let v6 = f.cond_loop(1); println(f"  v={v6}");
    println("loop_zero"); let g = E.A(mk(7)); let v7 = g.cond_loop(0); println(f"  v={v7}");
    println("arm_taken"); let h = E.A(mk(8)); let v8 = h.cond_arm(1); println(f"  v={v8}");
    println("arm_other"); let i = E.A(mk(9)); let v9 = i.cond_arm(3); println(f"  v={v9}");
    println("temp_true"); let v10 = E.A(mk(10)).cond_match(true); println(f"  v={v10}");
    println("temp_false"); let v11 = E.A(mk(11)).cond_match(false); println(f"  v={v11}");
    println("struct_true"); let j = S { r: mk(12), n: 1 }; let v12 = j.cond_struct(true); println(f"  v={v12}");
    println("struct_false"); let k = S { r: mk(13), n: 2 }; let v13 = k.cond_struct(false); println(f"  v={v13}");
    println("both_arms"); let l = S { r: mk(14), n: 3 }; let v14 = l.both_arms(false); println(f"  v={v14}");
    println("top_let"); let m = E.A(mk(15)); let v15 = m.top_let(); println(f"  v={v15}");
    println("plain"); let n = E.A(mk(16)); let v16 = n.plain(true); println(f"  v={v16}");
    println("borrowed"); let o = E.A(mk(17)); let v17 = o.borrowed(true); println(f"  v={v17}");
    println("end");
}
"#),
        r#"enum_true
  dE
  dR1
  v=1
enum_false
  dR2
  dE
  v=102
bare_true
  dE
  dR3
  v=7
bare_false
  dE
  v=0
mut_true
  dE
  dR5
  v=5
loop_once
  dE
  dR6
  v=9
loop_zero
  dE
  v=0
arm_taken
  dE
  dR8
  v=1
arm_other
  dE
  v=2
temp_true
  dE
  dR10
  v=10
temp_false
  dR11
  dE
  v=111
struct_true
  dS1
  dR12
  v=1
struct_false
  dS2
  dR13
  v=102
both_arms
  dS3
  dR14
  v=53
top_let
  dE
  dR15
  v=15
plain
  dE
  dR16
  v=1
borrowed
  dE
  dR17
  v=1
end
"#
    );
}

/// B-2026-09-06-64 — the interpreter always ran this program; the row was a
/// COMPILER crash on it. Pinned here so the string the compiled twin now
/// produces is held to the backend that was right.
///
/// Twin of `tests/codegen.rs`'s `e2e_self_referential_struct_compiles`, pinned to the same string.
#[test]
fn test_self_referential_struct_compiles() {
    assert_eq!(
        run(r#"struct Node { id: i64, next: Option[Node], tag: String }
impl Drop for Node { fn drop(mut ref self) { println(f"  dN{self.id}") } }
struct Plain { id: i64, next: Option[Plain], tag: String }
struct Env { id: i64, inner: Option[Option[i64]] }
fn mkn(i: i64) -> Node { return Node { id: i, next: Option.None, tag: f"t{i}" }; }
fn mkp(i: i64) -> Plain { return Plain { id: i, next: Option.None, tag: f"p{i}" }; }
fn top(n: Node) -> i64 { let m = n; return m.id; }
fn read(n: Node) -> i64 { return n.id; }
fn topp(p: Plain) -> i64 { let m = p; return m.id; }
fn enve(e: Env) -> i64 { return e.id; }
impl Node { fn take(self) -> i64 { let m = self; return m.id; } }

fn main() {
    println("bare_local"); let a = mkp(1); println(f"  v={a.id}");
    println("drop_local"); let b = mkn(2); println(f"  v={b.id}");
    println("free_fn_rebind"); println(f"  v={top(mkn(3))}");
    println("free_fn_read"); println(f"  v={read(mkn(4))}");
    println("plain_struct_rebind"); println(f"  v={topp(mkp(5))}");
    println("owned_self_rebind"); println(f"  v={mkn(6).take()}");
    println("boxed_envelope"); println(f"  v={enve(Env { id: 7, inner: Option.Some(Option.Some(8)) })}");
    println("end");
}
"#),
        r#"bare_local
  v=1
drop_local
  v=2
  dN2
free_fn_rebind
  dN3
  v=3
free_fn_read
  dN4
  v=4
plain_struct_rebind
  v=5
owned_self_rebind
  dN6
  v=6
boxed_envelope
  v=7
end
"#
    );
}

/// B-2026-09-07-6 — the interpreter was right on every cell of this row (its
/// tuple walk is value-driven, so it never needed the callee's declared types);
/// the twin holds the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_whole_tuple_argument_to_a_method`, pinned to the same string.
#[test]
fn test_whole_tuple_argument_to_a_method() {
    assert_eq!(
        run(r#"struct Q { id: i64, name: String }
impl Drop for Q { fn drop(mut ref self) { println(f"  dQ{self.id}") } }
fn mkq(i: i64) -> Q { return Q { id: i, name: f"q{i}" }; }
struct Hold { n: i64 }
impl Hold { fn thrut(ref self, t: (Q, i64)) -> (Q, i64) { return t; } }
impl Hold { fn mkt(ref self) -> (Q, i64) { return (mkq(4), 1); } }
impl Hold { fn eatt(ref self, t: (Q, i64)) -> i64 { return t.1; } }
impl Q { fn passt(t: (Q, i64)) -> (Q, i64) { return t; } }
fn passt(t: (Q, i64)) -> (Q, i64) { return t; }
fn main() {
  let h = Hold { n: 0 };
  println("method_tuple"); let a = h.thrut((mkq(1), 7)); println(f"  v={a.1}");
  println("assoc_tuple"); let b = Q.passt((mkq(2), 8)); println(f"  v={b.1}");
  println("free_tuple"); let c = passt((mkq(3), 9)); println(f"  v={c.1}");
  println("method_mints"); let d = h.mkt(); println(f"  v={d.1}");
  println("method_eats"); println(f"  v={h.eatt((mkq(5), 2))}");
  println("named_tuple_method"); let t = (mkq(6), 3); let e = h.thrut(t); println(f"  v={e.1}");
  println("named_tuple_assoc"); let u = (mkq(7), 4); let f = Q.passt(u); println(f"  v={f.1}");
  println("end");
}
"#),
        r#"method_tuple
  v=7
  dQ1
assoc_tuple
  v=8
  dQ2
free_tuple
  v=9
  dQ3
method_mints
  v=1
  dQ4
method_eats
  dQ5
  v=2
named_tuple_method
  v=3
  dQ6
named_tuple_assoc
  v=4
  dQ7
end
"#
    );
}

/// B-2026-09-13-27 — a tuple literal carrying a FIELD PROJECTED out of a
/// local (`let w = (t.r, 1);`) runs the moved leaf's `Drop` body ONCE, and at
/// the binding that owns it rather than at the source.
///
/// This backend ran it TWICE: `eval_struct_literal`'s field loop has stood
/// the source's field-bodies walk down since B-2026-09-01-17, and the tuple
/// literal's arm simply never asked, so `t`'s walk reached the moved-out leaf
/// and fired it on top of the owner's.
///
/// The `pre` / `post` markers are load-bearing and not decoration: they are
/// what distinguishes "one body" from "one body at the right time", which is
/// the distinction the row's count-only measurement could not make. Cell 5 (the
/// UNMOVED sibling field) and the whole-local controls pin the other side —
/// standing a source's walk down must not take a field that never moved.
///
/// The last cell is the STRUCT-literal spelling of cell 1, fixed by
/// B-2026-09-01-17 and asserted here as the reference the tuple spelling now
/// matches. Twin in the other backend's suite under the same name, with the
/// same table, so a one-sided change shows up as a diff rather than as a drift.
#[test]
fn test_tuple_literal_of_a_projected_field_runs_one_body_at_the_owner() {
    let hdr = "struct D { a: String, b: i64 }\n\
               impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
               struct W { r: D, s: D, b: i64 }\n\
               struct V { r: D, b: i64 }\n\
               fn pay() -> String { return \"heap\"; }\n\
               fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
               fn mkw(n: i64) -> W { return W { r: mkd(n), s: mkd(n + 100), b: n }; }\n";
    for (label, stmts, want) in [
        (
            "projected field, read later",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = (t.r, 1);\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "projected field through an if",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = if n == 0 { (t.r, 1) } else { (mkd(2), 2) };\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "projected field, never read",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = (t.r, 1);\n\
             println(\"post\");",
            "pre\ndD7\ndD107\npost\nend\n",
        ),
        (
            "projected field through an if, never read",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = if n == 0 { (t.r, 1) } else { (mkd(2), 2) };\n\
             println(\"post\");",
            "pre\ndD7\ndD107\npost\nend\n",
        ),
        (
            "the SIBLING field projected instead",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = (t.s, 1);\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\ndD7\npost\nidx1\ndD107\nend\n",
        ),
        (
            // B-2026-09-14-16 — was `pre post idx1 dD7 end`, which PINNED the
            // loss: `s`'s body ran nowhere, because a fresh temp has no
            // binding whose field walk survives the move-out. It now runs at
            // the projection, which is where the temp's live range ends —
            // exactly where cell 1's NAMED source prints it. The two cells
            // were the row's own oracle pair and now agree on the sibling's
            // position.
            "fresh-temp projection, no named source",
            "println(\"pre\");\n\
             let w = (mkw(7).r, 1);\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            // B-2026-09-14-16 — the projection inside an `if` ARM moved too, and
            // had to: codegen reaches its per-element consuming site through the
            // taken arm, so leaving the interpreter out of the branch arms was
            // measured as a fresh run-vs-build divergence rather than a
            // conservative omission. The stash is keyed on the projection's object
            // SPAN, so descending into both arms is safe without knowing which ran.
            "fresh-temp projection through an if",
            "println(\"pre\");\n\
             let w = if n == 0 { (mkw(7).r, 1) } else { (mkd(2), 2) };\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: whole-local element",
            "let d = mkd(7);\n\
             println(\"pre\");\n\
             let w = (d, 1);\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: whole-local element through an if",
            "let d = mkd(7);\n\
             println(\"pre\");\n\
             let w = if n == 0 { (d, 1) } else { (mkd(2), 2) };\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: fresh element through an if",
            "println(\"pre\");\n\
             let w = if n == 0 { (mkd(7), 1) } else { (mkd(2), 2) };\n\
             println(\"post\");\n\
             println(f\"idx{w.1}\");",
            "pre\npost\nidx1\ndD7\nend\n",
        ),
        (
            "control: the STRUCT-literal sibling of cell 1",
            "let t = mkw(7);\n\
             println(\"pre\");\n\
             let w = V { r: t.r, b: 1 };\n\
             println(\"post\");\n\
             println(f\"idx{w.b}\");",
            "pre\ndD107\npost\nidx1\ndD7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 0;\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-01-12 — the two repairs the contradicting-suffix diagnostic
/// offers both agree across every backend.
///
/// The row's own program (`Option[f32] = Option.Some(0.1f64)`) is now a type
/// error, so there is nothing left to run for it — which is the point: the
/// backends disagreed only on programs the checker can reject. What still has
/// to hold is that the fix-it does not trade a silent narrowing for a
/// run-vs-build split, the same obligation
/// `test_float_narrowing_as_cast_rounds_to_the_target_width` states for the
/// `as` the earlier gate recommends.
///
/// So both recommended spellings are pinned — drop the suffix and let the
/// destination type the literal, or keep it and add `as f32` — together with
/// the widening case that stays legal. `16777217` is there because it is not
/// representable in f32: before the fix `let c: f32 = 16777217.0f64` bound it
/// unrounded on all four surfaces, so the value doubles as the witness that
/// the annotation is now honoured rather than merely agreed upon.
/// Verified byte-identical under `karac run --interp`, `karac run`,
/// `karac build`, and `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn test_contradicting_suffix_repairs_agree_across_backends() {
    assert_eq!(
        run(
            r#"fn main() {
    let a: Option[f32] = Option.Some(0.1);
    println(f"{a}");
    let b: Option[f32] = Option.Some(0.1f64 as f32);
    println(f"{b}");
    let c: f32 = 16777217.0;
    println(f"{c}");
    let d: f32 = 16777217.0f64 as f32;
    println(f"{d}");
    let w: f64 = 0.1f32;
    println(f"{w}");
}
"#
        ),
        "Some(0.10000000149011612)\nSome(0.10000000149011612)\n16777216\n16777216\n0.10000000149011612\n"
    );
}

/// B-2026-09-06-7 — the two-step destructure of a nested tuple field off a
/// LOCAL: `let h = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) =
/// inner; let m: R = r;` ran `dR9` TWICE on every compiled backend, one of them
/// BEFORE the live read (`dR9 l3 9 dR9` against the interpreter's `l3 9 dR9`).
/// The early body was the struct's OWN walk, not the leaf's: the tuple-typed leaf
/// arm of `place_source_tuple_leaf_cleanups` handed `inner` the element bodies but
/// never recorded the element in `took_bodies`, so the B-2026-09-02-43 disarm
/// left `h`'s `NestedTuple` walk descending into `pe.0` and running the inner
/// struct's body at `h`'s NLL death, one statement later. The FIRST destructure
/// alone already doubled (`let (inner, y) = h.pe; println(inner.0.id)`); the
/// second step over the leaf (`let (r, x) = inner`) needs nothing of its own —
/// it moves `inner` whole, and the move-out suppression retires its walk before
/// the leaves take over (a disarm added there was measured inert by ablation).
///
/// THE CONTROLS: `flat` (the struct-typed leaf arm, which did record its index),
/// `plainlocal` / `fromcall` / `wholecopy` (a tuple LOCAL source, whose walk does
/// not descend into a nested element) and `param3` (the owned-param source,
/// B-2026-09-02-41) were one body each before and must stay so.
///
/// Twin of `tests/codegen.rs`'s `e2e_two_step_nested_tuple_field_destructure_runs_one_body`, pinned to the same string.
#[test]
fn test_two_step_nested_tuple_field_destructure_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
struct H3 { pe: (((R, i64), i64), i64) }
fn mkpair() -> (R, i64) { return (mk(11), 1) }

fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"  l3 {m.id}") }
fn norebind() { let h: H2 = H2 { pe: ((mk(7), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f"  nr {r.id}") }
fn viacall() { let h: H2 = H2 { pe: ((mk(4), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let d = consume(r); println(f"  vc {d}") }
fn three() { let h: H3 = H3 { pe: (((mk(3), 1), 2), 3) }; let (mid, z) = h.pe; let (inner, y) = mid; let (r, x) = inner; let m: R = r; println(f"  t3 {m.id}") }
fn leftin() { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; println(f"  li {x}") }
fn nestedblock() { let h: H2 = H2 { pe: ((mk(13), 1), 2) }; let (inner, y) = h.pe; { let (r, x) = inner; let m: R = r; println(f"  nb {m.id}") } println("  after") }
fn flat() { let h: H1 = H1 { pe: (mk(8), 1) }; let (r, k) = h.pe; let m: R = r; println(f"  fl {m.id}") }
fn plainlocal() { let t: ((R, i64), i64) = ((mk(6), 1), 2); let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f"  pl {m.id}") }
fn fromcall() { let inner = mkpair(); let (r, x) = inner; let m: R = r; println(f"  fc {m.id}") }
fn wholecopy() { let h: H2 = H2 { pe: ((mk(12), 1), 2) }; let t = h.pe; let (inner, y) = t; let (r, x) = inner; let m: R = r; println(f"  wc {m.id}") }
fn param3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"  p3 {m.id}") }

fn main() {
    println("local3"); local3();
    println("norebind"); norebind();
    println("viacall"); viacall();
    println("three"); three();
    println("leftin"); leftin();
    println("nestedblock"); nestedblock();
    println("flat"); flat();
    println("plainlocal"); plainlocal();
    println("fromcall"); fromcall();
    println("wholecopy"); wholecopy();
    println("param3"); param3(H2 { pe: ((mk(5), 1), 2) });
    println("end");
}
"#),
        r#"local3
  l3 9
  dR9
norebind
  nr 7
  dR7
viacall
  dR4
  vc 4
three
  t3 3
  dR3
leftin
  dR2
  li 1
nestedblock
  nb 13
  dR13
  after
flat
  fl 8
  dR8
plainlocal
  pl 6
  dR6
fromcall
  fc 11
  dR11
wholecopy
  wc 12
  dR12
param3
  p3 5
  dR5
end
"#
    );
}

/// B-2026-09-02-26 — A LOCAL TUPLE SCRUTINEE'S ELEMENT REBIND DOUBLES THE
/// `Drop` BODY, AND THE REPAIR IS THE OPPOSITE OF B-2026-08-31-7's.
///
/// `let t = (R { id: 6 }, 0); match t { (r, k) => { let m = r; … } }` ran
/// `dR6` twice on all four surfaces — agreed, and by one-value-one-body
/// agreed-wrong. One `R` is constructed, so one body is due.
///
/// -31-7 fixed the owned-PARAM spelling by making the element a VIEW: the
/// caller retains the value, so its walk stays the single owner and the rebind
/// inherits view-ness. A LOCAL has no caller to hand the body to, so widening
/// that marking here would have produced a body that runs NOWHERE. The repair
/// is the other direction — RETRACT the tuple's element walk for the moved
/// element and let the rebind own it, which is what the enum family already
/// does for a local scrutinee (`e6` below, correct before and after).
///
/// THE CONTROLS ARE THE POINT, because the fix WITHHOLDS a walk and the failure
/// mode of over-reaching is a body that never runs:
/// - `r6` — the same arm without the rebind. Read-only, so nothing is moved
///   out and the walk must stay the single owner. One body before and after.
/// - `s6` — the arm moves the element into a BY-VALUE CALLEE. One body before
///   and after, and the cell that picked the predicate: a bare-tuple element
///   has no owner of its own to transfer FROM, so treating the argument as
///   consuming (`binding_use::binding_only_read_through`, the enum family's
///   test) masked the walk and ran NO body at all — measured on both backends
///   independently. `consume_class::binding_only_borrowed` reads an
///   entry-copied argument as non-consuming and is what both sides now use.
/// - `t6` — a two-element tuple with only the FIRST rebound. `dR56 dR56 dR57`
///   before, `dR56 dR57` after: the mask is per element, not per tuple.
/// - `g6` — a GUARDED match whose consuming arm is NOT the one taken. The
///   codegen mask is static, so masking on ANY arm ran no body here at all;
///   requiring every binding arm to agree leaves this cell exactly as it was.
///   Its `k > 0` sibling still runs two — this row's bug surviving in the
///   mixed-guard cell, which an agreed-and-wrong answer is the documented
///   trade for against a body that vanishes.
/// - `ol` — the tuple OUTLIVES the match, with a statement after it. The
///   codegen retraction RE-REGISTERS the walk rather than swapping it in place,
///   and it runs inside the arm's region, so the untouched element's body could
///   have been pulled into the arm's cleanup frame. It fires at the tuple's own
///   NLL end (before `after match`), identically on all five surfaces.
/// - `e6` — the ENUM spelling of `p6`, correct before and after. It is what
///   shows the tuple family was behind the enum family rather than that a new
///   rule was invented.
///
/// `h6` and `l4` are the two shapes the fix reaches beyond the row's own:
/// a HEAP-carrying element (`dHx dHx` before — a genuine double free, which is
/// the severity question the row left open and `tests/memory_sanitizer.rs`'s
/// `asan_local_tuple_elem_rebind_frees_once` now pins), and an arm that hands
/// the element OUT of the function (`dR74` twice before). The owned-PARAM
/// escape and the `let (r, k) = t` destructure are NOT fixed here — they are
/// B-2026-09-02-24 and -25, whose machinery is elsewhere.
///
/// Twin of `tests/codegen.rs`'s `e2e_local_tuple_elem_rebind_runs_one_body`, pinned to the same string.
#[test]
fn test_local_tuple_elem_rebind_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct H { s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.s}") } }
enum E { A(R), B }

fn sink(r: R) -> i64 { r.id }

fn p6()  { let t = (R { id: 6 }, 0); match t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
fn e6()  { let t = E.A(R { id: 16 }); match t { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => {} } }
fn r6()  { let t = (R { id: 26 }, 0); match t { (r, k) => { println(f"  b{r.id}"); } } }
fn s6()  { let t = (R { id: 36 }, 0); match t { (r, k) => { println(f"  b{sink(r)}"); } } }
fn t6()  { let t = (R { id: 56 }, R { id: 57 }); match t { (r, q) => { let m = r; println(f"  b{m.id}"); } } }
fn h6()  { let t = (H { s: "x" }, 0); match t { (h, k) => { let m = h; println(f"  b{m.s}"); } } }
fn l4() -> R { let t = (R { id: 74 }, 0); match t { (r, k) => { r } } }
fn ol()  {
    let t = (R { id: 91 }, R { id: 92 });
    match t { (r, q) => { let m = r; println(f"  mv{m.id}") } }
    println("  after match");
}
fn g6(n: i64) {
    let t = (R { id: 86 }, n);
    match t {
        (r, k) if k > 0 => { let m = r; println(f"  mv{m.id}"); }
        (r, k) => { println(f"  rd{r.id}"); }
    }
}

fn main() {
    println("p6"); p6(); println("p6 end");
    println("e6"); e6(); println("e6 end");
    println("r6"); r6(); println("r6 end");
    println("s6"); s6(); println("s6 end");
    println("t6"); t6(); println("t6 end");
    println("h6"); h6(); println("h6 end");
    println("l4"); let q = l4(); println(f"  got{q.id}"); println("l4 end");
    println("ol"); ol(); println("ol end");
    println("g6"); g6(0); println("g6 end");
    println("done");
}
"#),
        r#"p6
  b6
dR6
p6 end
e6
  b16
dR16
e6 end
r6
  b26
dR26
r6 end
s6
  b36
dR36
s6 end
t6
  b56
dR56
dR57
t6 end
h6
  bx
dHx
h6 end
l4
  got74
dR74
l4 end
ol
  mv91
dR91
dR92
  after match
ol end
g6
  rd86
dR86
g6 end
done
"#
    );
}

/// B-2026-09-03-32 / B-2026-09-04-23 / B-2026-09-05-25 — interpreter twin of
/// `tests/codegen.rs`'s
/// `e2e_destructure_discard_dies_in_the_statement_and_masks_die_with_the_block`,
/// same program and string. The interpreter was the reference for every cell
/// but `three`, where its discards ran in pattern order and now run in reverse
/// declaration order (design.md's field drop order).
#[test]
fn test_destructure_discard_dies_in_the_statement_and_masks_die_with_the_block() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Two { a: R, b: R }
struct Ho2 { a: R, b: Option[R] }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn pDes(h: Two) { let Two { a, b: _ } = h; println("in") }
fn main() {
    { let h: Two = Two { a: mk(1), b: mk(101) }; let Two { a, b: _ } = h; println("one") }
    { let h: Two = Two { a: mk(2), b: mk(102) }; let Two { a: _, b } = h; println("two") }
    { let h: Two = Two { a: mk(3), b: mk(103) }; let Two { a: _, b: _ } = h; println("three") }
    { let h: Two = Two { a: mk(4), b: mk(104) }; let Two { a, b: _ } = h; println(f"use{a.id}"); println("four") }
    { let h: Two = Two { a: mk(5), b: mk(105) }; let Two { a, b } = h; println("five") }
    { let h: Two = Two { a: mk(6), b: mk(106) }; println("six") }
    { pDes(Two { a: mk(7), b: mk(107) }); println("seven") }
    { let h: Two = Two { a: mk(8), b: mk(108) }; let Two { a, b: _ } = h; let q: Two = Two { a: mk(9), b: mk(109) }; println("eight") }
    { let h: Ho2 = Ho2 { a: mk(10), b: Option.Some(mk(110)) }; let Ho2 { a, b: _ } = h; println("nine") }
    { let h: Two = Two { a: mk(11), b: mk(111) }; let Two { a: _, b } = h; println("ten") }
    println("end")
}
"#),
        "dR101\ndR1\none\ndR2\ndR102\ntwo\ndR103\ndR3\nthree\ndR104\nuse4\ndR4\nfour\ndR105\ndR5\nfive\ndR106\ndR6\nsix\nin\ndR107\ndR7\nseven\ndR108\ndR8\ndR109\ndR9\neight\ndR110\ndR10\nnine\ndR11\ndR111\nten\nend\n"
    );
}

#[test]
fn test_param_destructure_leaf_rebind_runs_body_once() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rebind(h: HoRes)    { let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rebindu(h: HoRes)   { let HoRes { a, b } = h; let c = b; println("  m") }
fn rebindcall(h: HoRes){ let HoRes { a, b } = h; let c = b;
                         match c { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn orebind(h: HoOpt)   { let HoOpt { a, b } = h; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn prebind(w: WrapR)   { let HoRes { a, b } = w.inner; let c = b;
                         match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn porebind(w: WrapO)  { let HoOpt { a, b } = w.inner; let c = b;
                         match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn direct(h: HoRes)    { let HoRes { a, b } = h;
                         match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn twice(h: HoRes)     { let HoRes { a, b } = h; let c = b; let d = c;
                         match d { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("rebind");     rebind(HoRes { a: mk(1), b: Result.Ok(mk(101)) })
  println("rebindu");    rebindu(HoRes { a: mk(2), b: Result.Ok(mk(102)) })
  println("rebindcall"); rebindcall(HoRes { a: mk(3), b: Result.Ok(mk(103)) })
  println("orebind");    orebind(HoOpt { a: mk(4), b: Option.Some(mk(104)) })
  println("prebind");    prebind(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("porebind");   porebind(WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } })
  println("direct");     direct(HoRes { a: mk(7), b: Result.Ok(mk(107)) })
  println("twice");      twice(HoRes { a: mk(8), b: Result.Ok(mk(108)) })
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"rebind
  okt101
dR101/t101
dR1/t1
rebindu
  m
dR102/t102
dR2/t2
rebindcall
  eat103
dR103/t103
dR3/t3
orebind
  okt104
dR104/t104
dR4/t4
prebind
  okt105
dR105/t105
dR5/t5
porebind
  okt106
dR106/t106
dR6/t6
direct
  okt107
dR107/t107
dR7/t7
twice
  okt108
dR108/t108
dR8/t8
done
"#
    );
}

#[test]
fn place_struct_arg_escaping_field_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Cd { r: R, z: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn zEsc(h: Cd) -> i64 { println("in5"); return h.z; }
struct Dd { a: R, b: R }
fn dEsc(h: Dd) -> R { let Dd { a, b } = h; println("in2"); return a; }
fn pEsc(h: Cd) -> R { println("in3"); return h.r; }
struct Gd[T] { r: T, z: i64 }
fn gEsc[T](h: Gd[T]) -> T { let Gd { r, z } = h; println("in4"); return r; }
fn main() {
  let g1 = Cd { r: mk(13), z: 9 };  let o1 = cEsc(g1); println(f"got{o1.id}");
  let g2 = Dd { a: mk(31), b: mk(32) }; let o2 = dEsc(g2); println(f"got{o2.id}");
  let g3 = Cd { r: mk(41), z: 9 };  let o3 = pEsc(g3); println(f"got{o3.id}");
  let g4 = Gd { r: mk(81), z: 9 };  let o4 = gEsc(g4); println(f"got{o4.id}");
  let g5 = Cd { r: mk(91), z: 9 };  let _ = cEsc(g5); println("after");
  let g6 = Cd { r: mk(51), z: 5 };  let v6 = zEsc(g6); println(f"gotz{v6}");
  println("end");
}
"#),
        "in\ngot13\ndR13\nin2\ndR32\ngot31\ndR31\nin3\ngot41\ndR41\nin4\ngot81\ndR81\nin\ndR91\nafter\nin5\ndR51\ngotz5\nend\n",
        "each escaping field dies once, at its owner in the caller; the \
         non-escaping sibling `b` keeps its body inside the call"
    );
}

/// B-2026-09-05-36 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_destructured_part_or_bare_param_handed_to_a_taking_callee_has_one_owner`,
/// same program and string. Both backends consult the same two predicates, so
/// the cells were agreed-and-wrong (two bodies) here exactly as compiled, and
/// move together now.
#[test]
fn test_destructured_part_or_bare_param_handed_to_a_taking_callee_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { r: R, n: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
fn wrapw(x: R) -> W { return W { r: x, n: 1 } }
fn t_fwd_let(t: (R, i64)) -> R { let (r, k) = t; wrap(r) }
fn t_fwd_let_ret(t: (R, i64)) -> R { let (r, k) = t; return wrap(r); }
fn t_stash_let(t: (R, i64), v: mut ref Vec[R]) -> i64 { let (r, k) = t; stash(r, v); k }
fn t_consume_let(t: (R, i64)) -> i64 { let (r, k) = t; consume(r) }
fn t_fwdw_let(t: (R, i64)) -> W { let (r, k) = t; wrapw(r) }
fn s_fwd_let(w: W) -> R { let W { r, n } = w; wrap(r) }
fn s_stash_let(w: W, v: mut ref Vec[R]) -> i64 { let W { r, n } = w; stash(r, v); n }
fn b_stash(x: R, v: mut ref Vec[R]) { stash(x, v) }
fn b_stash_ret(x: R, v: mut ref Vec[R]) -> i64 { stash(x, v); 7 }
fn b_fwd(x: R) -> R { wrap(x) }
fn b_consume(x: R) -> i64 { consume(x) }
fn main() {
    { let a: R = t_fwd_let((mk(1), 0)); println(f"r{a.id}"); println("one") }
    { let a: R = t_fwd_let_ret((mk(2), 0)); println(f"r{a.id}"); println("two") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash_let((mk(3), 0), mut v); println(f"r{d} n{v.len()}"); println("three") }
    { let d: i64 = t_consume_let((mk(4), 0)); println(f"r{d}"); println("four") }
    { let w: W = t_fwdw_let((mk(5), 0)); println(f"r{w.r.id}"); println("five") }
    { let a: R = s_fwd_let(W { r: mk(6), n: 1 }); println(f"r{a.id}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = s_stash_let(W { r: mk(7), n: 1 }, mut v); println(f"r{d} n{v.len()}"); println("seven") }
    { let mut v: Vec[R] = []; b_stash(mk(8), mut v); println(f"n{v.len()}"); println("eight") }
    { let mut v: Vec[R] = []; let d: i64 = b_stash_ret(mk(9), mut v); println(f"r{d} n{v.len()}"); println("nine") }
    { let a: R = b_fwd(mk(10)); println(f"r{a.id}"); println("ten") }
    { let d: i64 = b_consume(mk(11)); println(f"r{d}"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let a: R = t_fwd_let(t); println(f"r{a.id}"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let mut v: Vec[R] = []; let d: i64 = t_stash_let(t, mut v); println(f"r{d} n{v.len()}"); println("fourteen") }
    { let x: R = mk(15); let mut v: Vec[R] = []; b_stash(x, mut v); println(f"n{v.len()}"); println("fifteen") }
    { let w: W = W { r: mk(16), n: 1 }; let a: R = s_fwd_let(w); println(f"r{a.id}"); println("sixteen") }
    println("end")
}
"#),
        "r1\ndR1\none\nr2\ndR2\ntwo\nr0 n1\ndR3\nthree\ndR4\nr4\nfour\nr5\ndR5\nfive\nr6\ndR6\nsix\nr1 n1\ndR7\nseven\nn1\ndR8\neight\nr7 n1\ndR9\nnine\nr10\ndR10\nten\ndR11\nr11\neleven\nr13\ndR13\nthirteen\nr0 n1\ndR14\nfourteen\nn1\ndR15\nfifteen\nr16\ndR16\nsixteen\nend\n"
    );
}

/// B-2026-09-05-17 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_forwarded_place_struct_arg_field_handed_back_has_one_owner`, same
/// program and string. Both backends ask the same part-path predicate, so the
/// cells were agreed-and-wrong here as compiled; the interpreter additionally
/// runs its place-argument masks ahead of the whole-argument stand-down now,
/// which is what fixed the METHOD cell (`thirteen`).
#[test]
fn test_forwarded_place_struct_arg_field_handed_back_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct Cd { r: R, z: i64 }
struct W { r: R, n: i64 }
fn cEsc(h: Cd) -> R { let Cd { r, z } = h; println("in"); return r; }
fn cProj(h: Cd) -> R { return h.r; }
fn tEsc(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn fwd(g: Cd) -> R { return cEsc(g); }
fn fwd_tail(g: Cd) -> R { cEsc(g) }
fn fwd_proj(g: Cd) -> R { return cProj(g); }
fn fwd2(g: Cd) -> R { return fwd(g); }
fn fwd_t(t: (R, i64)) -> R { return tEsc(t); }
fn fwd_wrap(g: Cd) -> W { return W { r: cEsc(g), n: 1 }; }
fn fwd_cond(g: Cd, c: bool) -> R { if c { return cEsc(g); } return mk(99); }
fn fwd_let(g: Cd) -> R { let x: R = cEsc(g); return x; }
fn fwd_i(g: Cd) -> i64 { let x: R = cEsc(g); return x.id; }
struct H { n: i64 }
impl H { fn m_fwd(ref self, g: Cd) -> R { return cEsc(g); } }
fn main() {
    let h: H = H { n: 1 };
    { let g: Cd = Cd { r: mk(61), z: 9 }; let out: R = fwd(g); println(f"got{out.id}"); println("one") }
    { let g: Cd = Cd { r: mk(62), z: 9 }; let out: R = cEsc(g); println(f"got{out.id}"); println("two") }
    { let g: Cd = Cd { r: mk(63), z: 9 }; let out: R = fwd_tail(g); println(f"got{out.id}"); println("three") }
    { let g: Cd = Cd { r: mk(64), z: 9 }; let out: R = fwd_proj(g); println(f"got{out.id}"); println("four") }
    { let g: Cd = Cd { r: mk(65), z: 9 }; let out: R = fwd2(g); println(f"got{out.id}"); println("five") }
    { let t: (R, i64) = (mk(66), 0); let out: R = fwd_t(t); println(f"got{out.id}"); println("six") }
    { let g: Cd = Cd { r: mk(67), z: 9 }; let out: W = fwd_wrap(g); println(f"got{out.r.id}"); println("seven") }
    { let g: Cd = Cd { r: mk(69), z: 9 }; let out: R = fwd_cond(g, false); println(f"got{out.id}"); println("nine") }
    { let g: Cd = Cd { r: mk(70), z: 9 }; let out: R = fwd_let(g); println(f"got{out.id}"); println("ten") }
    { let g: Cd = Cd { r: mk(71), z: 9 }; let d: i64 = fwd_i(g); println(f"got{d}"); println("eleven") }
    { let out: R = fwd(Cd { r: mk(72), z: 9 }); println(f"got{out.id}"); println("twelve") }
    { let g: Cd = Cd { r: mk(73), z: 9 }; let out: R = h.m_fwd(g); println(f"got{out.id}"); println("thirteen") }
    println("end")
}
"#),
        "in\ngot61\ndR61\none\nin\ngot62\ndR62\ntwo\nin\ngot63\ndR63\nthree\ngot64\ndR64\nfour\nin\ngot65\ndR65\nfive\ngot66\ndR66\nsix\nin\ngot67\ndR67\nseven\ndR69\ngot99\ndR99\nnine\nin\ngot70\ndR70\nten\nin\ndR71\ngot71\neleven\nin\ngot72\ndR72\ntwelve\nin\ngot73\ndR73\nthirteen\nend\n"
    );
}

/// B-2026-09-03-4 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_destructured_part_returned_in_a_constructor_has_one_owner`, same
/// program and string. The `let` spellings doubled here exactly as compiled;
/// the struct-`match` spelling (`fifteen`) was right here and wrong compiled,
/// and both now come from the one part-path answer.
#[test]
fn test_destructured_part_returned_in_a_constructor_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B(i64) }
struct W { r: R, n: i64 }
struct Cd { r: R, z: i64 }
fn f_m(t: (R, i64)) -> Option[R] { match t { (r, k) => { Option.Some(r) } } }
fn f_l(t: (R, i64)) -> Option[R] { let (r, k) = t; Option.Some(r) }
fn f_lr(t: (R, i64)) -> Option[R] { let (r, k) = t; return Option.Some(r); }
fn f_res(t: (R, i64)) -> Result[R, i64] { let (r, k) = t; Result.Ok(r) }
fn f_e(t: (R, i64)) -> E { let (r, k) = t; E.A(r) }
fn f_em(t: (R, i64)) -> E { match t { (r, k) => { E.A(r) } } }
fn f_w(t: (R, i64)) -> W { let (r, k) = t; W { r: r, n: k } }
fn f_s(g: Cd) -> Option[R] { let Cd { r, z } = g; Option.Some(r) }
fn f_sm(g: Cd) -> Option[R] { match g { Cd { r, z } => { Option.Some(r) } } }
fn f_local() -> Option[R] { let t: (R, i64) = (mk(9), 0); let (r, k) = t; Option.Some(r) }
fn f_bare(x: R) -> Option[R] { Option.Some(x) }
fn f_two(t: (R, R)) -> Option[R] { let (a, b) = t; Option.Some(a) }
fn main() {
    { let g: Option[R] = f_m((mk(1), 0)); println("got"); println("one") }
    { let g: Option[R] = f_l((mk(2), 0)); println("got"); println("two") }
    { let g: Option[R] = f_lr((mk(3), 0)); println("got"); println("three") }
    { let g: Result[R, i64] = f_res((mk(4), 0)); println("got"); println("four") }
    { let g: E = f_e((mk(5), 0)); println("got"); println("five") }
    { let g: E = f_em((mk(6), 0)); println("got"); println("six") }
    { let g: W = f_w((mk(7), 0)); println(f"got{g.r.id}"); println("seven") }
    { let g: Option[R] = f_s(Cd { r: mk(8), z: 1 }); println("got"); println("eight") }
    { let g: Option[R] = f_local(); println("got"); println("nine") }
    { let g: Option[R] = f_bare(mk(10)); println("got"); println("ten") }
    { let g: Option[R] = f_two((mk(11), mk(12))); println("got"); println("eleven") }
    { let t: (R, i64) = (mk(13), 0); let g: Option[R] = f_m(t); println("got"); println("thirteen") }
    { let t: (R, i64) = (mk(14), 0); let g: Option[R] = f_l(t); println("got"); println("fourteen") }
    { let g: Option[R] = f_sm(Cd { r: mk(15), z: 1 }); println("got"); println("fifteen") }
    { let g: Option[R] = f_m((mk(16), 0)); match g { Option.Some(x) => { println(f"x{x.id}") }, Option.None => { println("none") } } println("sixteen") }
    println("end")
}
"#),
        "dR1\ngot\none\ndR2\ngot\ntwo\ndR3\ngot\nthree\ndR4\ngot\nfour\ndR5\ngot\nfive\ndR6\ngot\nsix\ngot7\ndR7\nseven\ndR8\ngot\neight\ndR9\ngot\nnine\ndR10\ngot\nten\ndR12\ndR11\ngot\neleven\ndR13\ngot\nthirteen\ndR14\ngot\nfourteen\ndR15\ngot\nfifteen\nx16\ndR16\nsixteen\nend\n"
    );
}

/// B-2026-09-02-41 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_two_step_destructure_of_a_nested_tuple_field_has_one_owner`, same
/// program and string. The row measured this backend right on a no-heap
/// `R`; with a heap-carrying one it ran NO body at all — the struct-field
/// walk (`drop_user_drop_fields_of_value`) and its gate
/// (`field_value_carries_user_drop`) both stopped at a nested tuple element
/// — so the cells with no destructure at all (`v241i`'s scope-end local and
/// argument spellings, folded into `four` and `eight` here) were this
/// backend's own.
#[test]
fn test_two_step_destructure_of_a_nested_tuple_field_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
struct H3 { pe: (((R, i64), i64), i64) }
fn v3(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"v3 {m.id}") }
fn v3_read(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println(f"v3r {r.id}") }
fn v3_unread(h: H2) { let (inner, y) = h.pe; let (r, x) = inner; println("v3u") }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_one(h: H2) { let (inner, y) = h.pe; println("v3o") }
fn v3_inner_move(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn flat(h: H1) { let (r, k) = h.pe; let m: R = r; println(f"flat {m.id}") }
fn flat_read(h: H1) { let (r, k) = h.pe; println(f"flatr {r.id}") }
fn deep(h: H3) { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; let m: R = r; println(f"deep {m.id}") }
fn local3() { let h: H2 = H2 { pe: ((mk(9), 1), 2) }; let (inner, y) = h.pe; let (r, x) = inner; let m: R = r; println(f"l3 {m.id}") }
fn main() {
    { v3(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3_read(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3_unread(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3_one(H2 { pe: ((mk(5), 1), 2) }); println("five") }
    { flat(H1 { pe: (mk(7), 1) }); println("seven") }
    { flat_read(H1 { pe: (mk(8), 1) }); println("eight") }
    { deep(H3 { pe: (((mk(10), 1), 2), 3) }); println("ten") }
    { let h: H2 = H2 { pe: ((mk(11), 1), 2) }; v3(h); println("eleven") }
    println("end")
}
"#),
        "v3 1\ndR1\none\nv3r 2\ndR2\ntwo\nv3u\ndR3\nthree\nv3o\ndR5\nfive\nflat 7\ndR7\nseven\nflatr 8\ndR8\neight\ndeep 10\ndR10\nten\nv3 11\ndR11\neleven\nend\n"
    );
}

/// B-2026-09-06-6 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_whole_rebind_of_a_tuple_param_view_runs_one_body`, same program and
/// string. This backend was right on every cell before the fix — its tuple
/// walk is value-driven, so a rebind never re-arms a body — and the twin
/// pins that so the row's parity is asserted from both sides.
#[test]
fn test_whole_rebind_of_a_tuple_param_view_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H1 { pe: (R, i64) }
fn take(t: (R, i64)) { println(f"take {t.0.id}") }
fn v3m(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println(f"v3m {z.0.id}") }
fn v3m_unread(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; println("v3mu") }
fn v3m_destr(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let (r, x) = z; println(f"v3md {r.id}") }
fn v3m_twice(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; let w: (R, i64) = z; println(f"v3mt {w.0.id}") }
fn v3m_ret(h: H2) -> (R, i64) { let (inner, y) = h.pe; let z: (R, i64) = inner; return z; }
fn v3m_untyped(h: H2) { let (inner, y) = h.pe; let z = inner; println(f"vu {z.0.id}") }
fn v3m_take(h: H2) { let (inner, y) = h.pe; let z: (R, i64) = inner; take(z); println("vt") }
fn flat_m(h: H1) { let p: (R, i64) = h.pe; println(f"fm {p.0.id}") }
fn t_m(t: (R, i64)) { let z: (R, i64) = t; println(f"tm {z.0.id}") }
fn t_destr(t: (R, i64)) { let z: (R, i64) = t; let (r, n) = z; println(f"td {r.id} {n}") }
fn t_take(t: (R, i64)) { let z: (R, i64) = t; take(z); println("tt") }
fn t_ret(t: (R, i64)) -> (R, i64) { let z: (R, i64) = t; return z; }
fn t_untyped(t: (R, i64)) { let z = t; println(f"tu {z.0.id}") }
fn main() {
    { v3m(H2 { pe: ((mk(1), 1), 2) }); println("one") }
    { v3m_unread(H2 { pe: ((mk(2), 1), 2) }); println("two") }
    { v3m_destr(H2 { pe: ((mk(3), 1), 2) }); println("three") }
    { v3m_twice(H2 { pe: ((mk(4), 1), 2) }); println("four") }
    { let a: (R, i64) = v3m_ret(H2 { pe: ((mk(5), 1), 2) }); println(f"got{a.0.id}"); println("five") }
    { flat_m(H1 { pe: (mk(6), 1) }); println("six") }
    { t_m((mk(7), 1)); println("seven") }
    { let h: H2 = H2 { pe: ((mk(8), 1), 2) }; v3m(h); println("eight") }
    { v3m_untyped(H2 { pe: ((mk(9), 1), 2) }); println("nine") }
    { v3m_take(H2 { pe: ((mk(10), 1), 2) }); println("ten") }
    { t_destr((mk(11), 11)); println("eleven") }
    { t_take((mk(12), 12)); println("twelve") }
    { let a: (R, i64) = t_ret((mk(13), 13)); println(f"got{a.0.id}"); println("thirteen") }
    { t_untyped((mk(14), 14)); println("fourteen") }
    { let t: (R, i64) = (mk(15), 15); t_m(t); println("fifteen") }
    println("end")
}
"#),
        "v3m 1\ndR1\none\nv3mu\ndR2\ntwo\nv3md 3\ndR3\nthree\nv3mt 4\ndR4\nfour\ngot5\ndR5\nfive\nfm 6\ndR6\nsix\ntm 7\ndR7\nseven\nv3m 8\ndR8\neight\nvu 9\ndR9\nnine\ntake 10\nvt\ndR10\nten\ntd 11 11\ndR11\neleven\ntake 12\ntt\ndR12\ntwelve\ngot13\ndR13\nthirteen\ntu 14\ndR14\nfourteen\ntm 15\ndR15\nfifteen\nend\n"
    );
}

/// B-2026-09-06-5 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_nested_tuple_element_handed_back_two_levels_deep_runs_one_body`,
/// same program and string. This backend masks the escaping path exactly,
/// at any depth (`escaping_field_paths` carries `#<i>` per tuple hop), and
/// was right on every cell before the fix; the twin pins that.
#[test]
fn test_nested_tuple_element_handed_back_two_levels_deep_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H2 { pe: ((R, i64), i64) }
struct H3 { pe: (((R, i64), i64), i64) }
struct H2b { pe: ((R, R), i64) }
struct S { r: R, n: i64 }
struct H4 { pe: ((S, i64), i64) }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn v3_ret_direct(h: H2) -> R { return h.pe.0.0; }
fn v3_ret_z(h: H2) -> R { let z: (R, i64) = h.pe.0; let (r, x) = z; return r; }
fn v4_ret(h: H3) -> R { let (mid, a) = h.pe; let (inner, b) = mid; let (r, c) = inner; return r; }
fn vb_ret0(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn vs_ret(h: H4) -> R { let (inner, y) = h.pe; let (s, x) = inner; let S { r, n } = s; return r; }
fn v3_ret_tuple(h: H2) -> (R, i64) { let (inner, y) = h.pe; return inner; }
fn main() {
    { let a: R = v3_ret(H2 { pe: ((mk(1), 1), 2) }); println(f"got{a.id}"); println("one") }
    { let a: R = v3_ret_direct(H2 { pe: ((mk(2), 1), 2) }); println(f"got{a.id}"); println("two") }
    { let a: R = v3_ret_z(H2 { pe: ((mk(3), 1), 2) }); println(f"got{a.id}"); println("three") }
    { let a: R = v4_ret(H3 { pe: (((mk(4), 1), 2), 3) }); println(f"got{a.id}"); println("four") }
    { let a: R = vb_ret0(H2b { pe: ((mk(5), mk(6)), 2) }); println(f"got{a.id}"); println("five") }
    { let a: R = vb_ret1(H2b { pe: ((mk(7), mk(8)), 2) }); println(f"got{a.id}"); println("six") }
    { let a: R = vs_ret(H4 { pe: ((S { r: mk(9), n: 1 }, 1), 2) }); println(f"got{a.id}"); println("seven") }
    { let a: (R, i64) = v3_ret_tuple(H2 { pe: ((mk(10), 1), 2) }); println(f"got{a.0.id}"); println("eight") }
    println("end")
}
"#),
        "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ndR6\ngot5\ndR5\nfive\ndR7\ngot8\ndR8\nsix\ngot9\ndR9\nseven\ngot10\ndR10\neight\nend\n"
    );
}

/// B-2026-09-06-10 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_named_local_handing_back_a_nested_part_runs_one_body`, same program
/// and string — and a REAL pin this time: this backend doubled every named
/// cell too, because its identifier arm keyed the mask by one top-level
/// field and dropped a deeper path, where its fresh-temp leg had masked the
/// same escape at full depth (`escaping_field_paths`, `#<i>` per tuple hop).
/// The deeper path now goes to the path-keyed `moved_out_nested_field_bodies`
/// with the same spelling, and `remove_field_at_path` steps through a tuple.
#[test]
fn test_named_local_handing_back_a_nested_part_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct H1 { pe: (R, i64) }
struct H2 { pe: ((R, i64), i64) }
struct S { r: R, n: i64 }
struct G { s: S, n: i64 }
struct H2b { pe: ((R, R), i64) }
fn flat_ret(h: H1) -> R { let (r, k) = h.pe; return r; }
fn flat_proj(h: H1) -> R { return h.pe.0; }
fn v3_ret(h: H2) -> R { let (inner, y) = h.pe; let (r, x) = inner; return r; }
fn g_ret(g: G) -> R { return g.s.r; }
fn g_destr(g: G) -> R { let G { s, n } = g; let S { r, n: m } = s; return r; }
fn vb_ret1(h: H2b) -> R { let (inner, y) = h.pe; let (r, s) = inner; return s; }
fn flat_in(h: H1) -> R { println("in"); let (r, k) = h.pe; println("mid"); return r; }
fn main() {
    { let h: H1 = H1 { pe: (mk(1), 1) }; let a: R = flat_ret(h); println(f"got{a.id}"); println("one") }
    { let h: H2 = H2 { pe: ((mk(2), 1), 2) }; let a: R = v3_ret(h); println(f"got{a.id}"); println("two") }
    { let g: G = G { s: S { r: mk(3), n: 1 }, n: 2 }; let a: R = g_ret(g); println(f"got{a.id}"); println("three") }
    { let a: R = flat_ret(H1 { pe: (mk(4), 1) }); println(f"got{a.id}"); println("four") }
    { let a: R = g_ret(G { s: S { r: mk(5), n: 1 }, n: 2 }); println(f"got{a.id}"); println("five") }
    { let h: H1 = H1 { pe: (mk(6), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println("six") }
    { let h: H1 = H1 { pe: (mk(7), 1) }; let a: R = flat_in(h); println("out"); println(f"got{a.id}"); println(f"h{h.pe.1}"); println("seven") }
    { let h: H1 = H1 { pe: (mk(8), 1) }; let a: R = flat_proj(h); println(f"got{a.id}"); println("eight") }
    { let g: G = G { s: S { r: mk(9), n: 1 }, n: 2 }; let a: R = g_destr(g); println(f"got{a.id}"); println("nine") }
    { let h: H2b = H2b { pe: ((mk(10), mk(11)), 2) }; let a: R = vb_ret1(h); println(f"got{a.id}"); println("ten") }
    println("end")
}
"#),
        "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ngot5\ndR5\nfive\nin\nmid\nout\ngot6\ndR6\nsix\nin\nmid\nout\ngot7\ndR7\nh1\nseven\ngot8\ndR8\neight\ngot9\ndR9\nnine\ndR10\ngot11\ndR11\nten\nend\n"
    );
}

/// B-2026-09-06-11 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_argument_handing_back_a_nested_part_runs_one_body`, same
/// program and string, and a real pin: this backend doubled every deep cell
/// too, named local and fresh temp alike, because every tuple-argument mask
/// it had was a flat top-level element index. The named local's deeper
/// path now goes to the path-keyed `moved_out_nested_field_bodies` and the
/// tuple binding's walk applies it; the fresh temp masks it out of the
/// value through `mask_struct_fields`. Both under the leaf gate
/// (`value_leaf_can_own`) — cells `eleven`/`twelve` are that gate's shape.
#[test]
fn test_tuple_argument_handing_back_a_nested_part_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
struct S { r: R, n: i64 }
fn tv_ret(t: ((R, i64), i64)) -> R { let (inner, y) = t; let (r, x) = inner; return r; }
fn tv_proj(t: ((R, i64), i64)) -> R { return t.0.0; }
fn flat_t(t: (R, i64)) -> R { let (r, k) = t; return r; }
fn ts_ret(t: (S, i64)) -> R { let (s, k) = t; return s.r; }
fn t3_ret(t: (((R, i64), i64), i64)) -> R { let (mid, a) = t; let (inner, b) = mid; let (r, c) = inner; return r; }
fn tb_ret1(t: ((R, R), i64)) -> R { let (inner, y) = t; let (a, b) = inner; return b; }
fn tv_read(t: ((R, i64), i64)) -> i64 { let (inner, y) = t; let (r, x) = inner; return r.id; }
fn main() {
    { let t: ((R, i64), i64) = ((mk(1), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println("one") }
    { let t: (R, i64) = (mk(2), 2); let a: R = flat_t(t); println(f"got{a.id}"); println("two") }
    { let t: (S, i64) = (S { r: mk(3), n: 1 }, 2); let a: R = ts_ret(t); println(f"got{a.id}"); println("three") }
    { let a: R = tv_ret(((mk(4), 1), 2)); println(f"got{a.id}"); println("four") }
    { let a: R = ts_ret((S { r: mk(5), n: 1 }, 2)); println(f"got{a.id}"); println("five") }
    { let t: ((R, i64), i64) = ((mk(6), 1), 2); let a: R = tv_proj(t); println(f"got{a.id}"); println("six") }
    { let t: ((R, i64), i64) = ((mk(7), 1), 2); let a: R = tv_ret(t); println(f"got{a.id}"); println(f"k{t.1}"); println("seven") }
    { let t: (((R, i64), i64), i64) = (((mk(8), 1), 2), 3); let a: R = t3_ret(t); println(f"got{a.id}"); println("eight") }
    { let t: ((R, R), i64) = ((mk(9), mk(10)), 2); let a: R = tb_ret1(t); println(f"got{a.id}"); println("nine") }
    { let a: R = tb_ret1(((mk(11), mk(12)), 2)); println(f"got{a.id}"); println("ten") }
    { let d: i64 = tv_read(((mk(13), 1), 2)); println(f"r{d}"); println("eleven") }
    { let t: ((R, i64), i64) = ((mk(14), 1), 2); let d: i64 = tv_read(t); println(f"r{d}"); println("twelve") }
    println("end")
}
"#),
        "got1\ndR1\none\ngot2\ndR2\ntwo\ngot3\ndR3\nthree\ngot4\ndR4\nfour\ngot5\ndR5\nfive\ngot6\ndR6\nsix\ngot7\ndR7\nk2\nseven\ngot8\ndR8\neight\ndR9\ngot10\ndR10\nnine\ndR11\ngot12\ndR12\nten\ndR13\nr13\neleven\ndR14\nr14\ntwelve\nend\n"
    );
}

/// B-2026-09-06-30 — a `let` DESTRUCTURE of a mixed struct literal
/// (`let s = S3 { a: r, b: mk(2) }; let S3 { a, b } = s;`, `r` a by-value
/// param) ran the view field's `Drop` body twice on ALL FOUR surfaces: the
/// literal's `param_view_struct_fields` record guarded `s`'s own walk, but
/// the destructure leaf `a` got a slot of its own beside the caller's walk.
/// `let_destructure_view_leaves` now hands those leaves to
/// `push_drops_for_stmt_except` and marks them views in
/// `owned_param_names_stack`, mirroring B-2026-09-06-22's match arm. Twin of
/// `tests/codegen.rs`'s
/// `e2e_let_destructure_of_a_mixed_struct_literal_runs_one_body`, same
/// program and string.
///
/// `one`..`three` direct / `a` read / unread, `four` rebound source, `five`
/// `let m = a` after the destructure, `six` the view in the other field,
/// `seven` a fresh literal (no view), `eight`/`nine` the tuple spelling
/// (`param_view_tuple_elems`) direct and re-bound.
#[test]
fn test_let_destructure_of_a_mixed_struct_literal_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn d_b(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(2) }; let S3 { a, b } = s; return b.id; }
fn d_a(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(4) }; let S3 { a, b } = s; return a.id; }
fn d_unread(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(6) }; let S3 { a, b } = s; return 1; }
fn d_rebind(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(8) }; let s2: S3 = s; let S3 { a, b } = s2; return b.id; }
fn d_rebind_leaf(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(10) }; let S3 { a, b } = s; let m: R = a; return m.id; }
fn d_swap(r: R) -> i64 { let s: S3 = S3 { a: mk(12), b: r }; let S3 { a, b } = s; return a.id; }
fn d_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(14), b: mk(15) }; let S3 { a, b } = s; return a.id; }
fn t_b(r: R) -> i64 { let t: (R, R) = (r, mk(19)); let (a, b) = t; return b.id; }
fn t_rebind_leaf(r: R) -> i64 { let t: (R, R) = (r, mk(21)); let (a, b) = t; let m: R = a; return m.id; }
fn main() {
    { let v: i64 = d_b(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = d_a(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = d_unread(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = d_rebind(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = d_rebind_leaf(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = d_swap(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = d_fresh(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = t_b(mk(18)); println(f"v={v}"); println("eight") }
    { let v: i64 = t_rebind_leaf(mk(20)); println(f"v={v}"); println("nine") }
    println("end")
}
"#),
        "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=3\ntwo\ndR6\ndR5\nv=1\nthree\ndR8\ndR7\nv=8\nfour\ndR10\ndR9\nv=9\nfive\ndR12\ndR11\nv=12\nsix\ndR15\ndR14\ndR13\nv=14\nseven\ndR19\ndR18\nv=19\neight\ndR21\ndR20\nv=20\nnine\nend\n"
    );
}
