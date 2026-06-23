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

#[test]
fn variant_payload_reflects_single_tuple_type() {
    // `Variant.payload` is the type name of a single-field tuple variant's
    // payload, or "" for a unit / multi-field variant.
    let src = "
enum E { None, Num(i64), Text(String), Pair(i64, i64) }
fn main() {
    let r = comptime {
        let mut s = \"\";
        for v in E.variants() { s = s + v.name + \"=\" + v.payload + \";\"; }
        s
    };
    println(r);
}";
    assert_eq!(
        karac::run_program(src),
        vec!["None=;Num=i64;Text=String;Pair=;\n"]
    );
}

#[test]
fn variant_payload_ty_reflects_on_the_payload_type() {
    // `Variant.payload_ty` is the payload as a `Type` pseudovalue, so comptime
    // code can reflect on it (`is_struct` / `is_enum` / `variants`).
    let src = "
struct Inner { v: i64 }
enum Tint { Red, Green }
enum E { None, Msg(Inner), Hue(Tint), Num(i64) }
fn main() {
    let r = comptime {
        let mut s = \"\";
        for v in E.variants() {
            if v.payload != \"\" {
                s = s + v.name + \":\" + v.payload_ty.name();
                if v.payload_ty.is_struct() { s = s + \"(struct)\"; }
                if v.payload_ty.is_enum() { s = s + \"(enum,\" + f\"{v.payload_ty.variants().len()}\" + \")\"; }
                s = s + \";\";
            }
        }
        s
    };
    println(r);
}";
    assert_eq!(
        karac::run_program(src),
        vec!["Msg:Inner(struct);Hue:Tint(enum,2);Num:i64;\n"]
    );
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

#[test]
fn key_type_and_value_type_peel_two_args() {
    // `key_type()` / `value_type()` peel the 1st / 2nd top-level generic arg,
    // respecting nested `<…>` so a `Vec` value keeps its comma-free form.
    let src = "
struct Inner { v: i64 }
struct H { a: Map[String, i64], b: Map[i32, Inner], c: Map[String, Vec[u8]] }
fn main() {
    let r = comptime {
        let mut s = \"\";
        for f in H.fields() {
            s = s + f.ty.key_type().name() + \"=>\" + f.ty.value_type().name();
            if f.ty.value_type().is_struct() { s = s + \"*\"; }
            s = s + \";\";
        }
        s
    };
    println(r);
}";
    assert_eq!(
        karac::run_program(src),
        vec!["String=>i64;i32=>Inner*;String=>Vec<u8>;\n"]
    );
}

// ── Field.attrs ─────────────────────────────────────────────────

#[test]
fn field_attrs_reflects_rendered_attributes() {
    // A field's attributes are exposed as `Field.attrs` (rendered strings);
    // a field without attributes has an empty list.
    let src = "
struct S {
    #[karac::proto(sint64)] x: i64,
    y: i64,
}
fn main() {
    let r = comptime {
        let mut s = \"\";
        for f in S.fields() {
            s = s + f.name + \":\" + f\"{f.attrs.len()}\";
            for a in f.attrs { s = s + \"=\" + a; }
            s = s + \";\";
        }
        s
    };
    println(r);
}";
    assert_eq!(
        karac::run_program(src),
        vec!["x:1=karac::proto(sint64);y:0;\n"]
    );
}

#[test]
fn field_attrs_renders_integer_argument() {
    // An integer-literal attribute argument (e.g. `#[karac::field(5)]`) renders
    // with its decimal value, so comptime code can read a field number.
    let src = "
struct S { #[karac::field(5)] x: i64, y: i64 }
fn main() {
    let r = comptime {
        let mut s = \"\";
        for f in S.fields() { for a in f.attrs { s = s + f.name + \"=\" + a + \";\"; } }
        s
    };
    println(r);
}";
    assert_eq!(karac::run_program(src), vec!["x=karac::field(5);\n"]);
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
