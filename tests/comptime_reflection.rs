//! Comptime `Type` reflection (substrate 2).
//!
//! Exercises the reflection API on a `Type` pseudovalue at comptime —
//! `name()`, `is_struct()` / `is_enum()`, `fields()` (with `Field.name` /
//! `Field.ty.name()`), `variants()` — both in the direct `TypeName.method()`
//! form and through a `comptime fn(comptime T: Type)` parameter. Plus the
//! `E_TYPE_VALUE_AT_RUNTIME` gate. Spec: deferred.md § Comptime — Types as
//! first-class values / Reflection API.

/// Typecheck a program (through desugar + resolve) and return the errors.
fn typecheck_errors(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    typed
        .errors
        .iter()
        .map(|e| e.message.clone())
        .collect::<Vec<_>>()
}

// ── name() / is_struct() / is_enum() ────────────────────────────

#[test]
fn type_name_reflects() {
    let src = "
struct Point { x: i64, y: i64 }
fn main() { println(comptime { Point.name() }); }";
    assert_eq!(karac::run_program(src), vec!["Point\n"]);
}

#[test]
fn is_struct_and_is_enum() {
    let src = "
struct Point { x: i64, y: i64 }
enum Color { Red, Green, Blue }
fn main() {
    println(comptime { Point.is_struct() });
    println(comptime { Point.is_enum() });
    println(comptime { Color.is_enum() });
}";
    assert_eq!(karac::run_program(src), vec!["true\n", "false\n", "true\n"]);
}

// ── fields() ────────────────────────────────────────────────────

#[test]
fn fields_count() {
    let src = "
struct Point { x: i64, y: i64 }
fn main() { println(comptime { Point.fields().len() }); }";
    assert_eq!(karac::run_program(src), vec!["2\n"]);
}

#[test]
fn fields_names_iterated() {
    let src = "
struct Point { x: i64, y: i64 }
fn main() {
    let names = comptime {
        let mut s = \"\";
        for f in Point.fields() { s = s + f.name + \";\"; }
        s
    };
    println(names);
}";
    assert_eq!(karac::run_program(src), vec!["x;y;\n"]);
}

#[test]
fn field_ty_name_chains() {
    // `field.ty` is itself a `Type` value, so `field.ty.name()` chains.
    let src = "
struct Mixed { count: i64, label: String }
fn main() {
    let tys = comptime {
        let mut s = \"\";
        for f in Mixed.fields() { s = s + f.ty.name() + \";\"; }
        s
    };
    println(tys);
}";
    // type_display renders i64 as `i64` and the string type as `String`.
    assert_eq!(karac::run_program(src), vec!["i64;String;\n"]);
}

// ── variants() ──────────────────────────────────────────────────

#[test]
fn variants_count_and_names() {
    let src = "
enum Color { Red, Green, Blue }
fn main() {
    println(comptime { Color.variants().len() });
    let names = comptime {
        let mut s = \"\";
        for v in Color.variants() { s = s + v.name + \";\"; }
        s
    };
    println(names);
}";
    assert_eq!(karac::run_program(src), vec!["3\n", "Red;Green;Blue;\n"]);
}

// ── derives() ───────────────────────────────────────────────────

#[test]
fn derives_reflects_declared_traits() {
    let src = "
#[derive(Eq)]
struct P { x: i64 }
struct Q { y: i64 }
fn main() {
    println(comptime { P.derives(\"Eq\") });
    println(comptime { P.derives(\"Hash\") });
    println(comptime { Q.derives(\"Eq\") });
}";
    assert_eq!(
        karac::run_program(src),
        vec!["true\n", "false\n", "false\n"]
    );
}

#[test]
fn derives_via_comptime_fn_param() {
    let src = "
#[derive(Eq)]
struct P { x: i64 }
comptime fn is_eq(comptime T: Type) -> bool { T.derives(\"Eq\") }
fn main() { println(comptime { is_eq(P) }); }";
    assert_eq!(karac::run_program(src), vec!["true\n"]);
}

#[test]
fn derives_arity_error() {
    // `derives` needs exactly one argument; calling it with none is an arity
    // error from the typechecker.
    let src = "
struct P { x: i64 }
fn main() { let _ = comptime { P.derives() }; }";
    let errs = typecheck_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("derives") && e.contains("one argument")),
        "expected a derives arity error; got: {errs:?}"
    );
}

// ── element_type() ──────────────────────────────────────────────

#[test]
fn element_type_peels_one_generic_arg() {
    // `element_type()` of a field's `Vec[T]` type yields `T` as a `Type`, with
    // `is_struct()` distinguishing a message element from a scalar.
    let src = "
struct Inner { v: i64 }
struct Holder { nums: Vec[i64], items: Vec[Inner], raw: Vec[u8] }
fn main() {
    let r = comptime {
        let mut s = \"\";
        for f in Holder.fields() {
            s = s + f.ty.element_type().name();
            if f.ty.element_type().is_struct() { s = s + \"*\"; }
            s = s + \";\";
        }
        s
    };
    println(r);
}";
    assert_eq!(karac::run_program(src), vec!["i64;Inner*;u8;\n"]);
}

#[test]
fn element_type_of_non_generic_is_identity() {
    let src = "
struct P { x: i64 }
fn main() { println(comptime { P.element_type().name() }); }";
    assert_eq!(karac::run_program(src), vec!["P\n"]);
}

// ── comptime fn with `comptime T: Type` parameter ───────────────

#[test]
fn comptime_fn_type_param() {
    let src = "
struct Point { x: i64, y: i64 }
comptime fn field_count(comptime T: Type) -> usize { T.fields().len() }
fn main() { println(comptime { field_count(Point) }); }";
    assert_eq!(karac::run_program(src), vec!["2\n"]);
}

// ── E_TYPE_VALUE_AT_RUNTIME ─────────────────────────────────────

#[test]
fn type_value_at_runtime_is_error() {
    // A runtime function may not take a `Type` parameter — `Type` values are
    // first-class only at compile time.
    let src = "
struct Point { x: i64, y: i64 }
fn describe(t: Type) -> i64 { 0 }
fn main() {}";
    let errs = typecheck_errors(src);
    assert!(
        errs.iter().any(|e| e.contains("E_TYPE_VALUE_AT_RUNTIME")),
        "expected E_TYPE_VALUE_AT_RUNTIME; got: {errs:?}"
    );
}

#[test]
fn comptime_type_param_is_allowed() {
    // The same `Type` parameter is fine when the parameter is `comptime`.
    let src = "
struct Point { x: i64, y: i64 }
comptime fn describe(comptime T: Type) -> i64 { 0 }
fn main() {}";
    let errs = typecheck_errors(src);
    assert!(
        !errs.iter().any(|e| e.contains("E_TYPE_VALUE_AT_RUNTIME")),
        "comptime Type param should be allowed; got: {errs:?}"
    );
}

#[test]
fn reflection_inside_comptime_is_clean() {
    // The same reflection inside comptime must NOT trip the runtime gate.
    let src = "
struct Point { x: i64, y: i64 }
fn main() { let _n = comptime { Point.name() }; }";
    let errs = typecheck_errors(src);
    assert!(
        !errs.iter().any(|e| e.contains("E_TYPE_VALUE_AT_RUNTIME")),
        "comptime reflection should not trip the runtime gate; got: {errs:?}"
    );
}
