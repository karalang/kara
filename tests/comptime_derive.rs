//! Comptime derive desugaring (substrate 4).
//!
//! `#[derive(X)]` on a struct/enum desugars to a call to a `comptime fn
//! derive_x(comptime T: Type) -> Vec[Item]` (lookup convention:
//! `derive_<snake(TraitName)>`); the items it returns — built with the
//! `ast.item(s)` quasi-quote builder — are spliced into the module after the
//! derive site. Built-in derives without a backing comptime fn are left to the
//! existing native handling. Spec: deferred.md § Comptime — Code generation
//! and derive desugaring.

/// Run parse → desugar → resolve → typecheck → lower → comptime and return the
/// comptime diagnostics.
fn comptime_diags(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    karac::comptime_eval(&mut parsed.program, &typed)
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

/// Typecheck (through desugar + resolve) and return the error messages.
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
    typed.errors.iter().map(|e| e.message.clone()).collect()
}

// ── basic generation + splice ───────────────────────────────────

#[test]
fn derive_generates_inherent_method() {
    // `#[derive(Describe)]` → `derive_describe(Widget)` returns an `impl Widget`
    // with a `describe` method, spliced after the struct. `main` calls it.
    let src = "
comptime fn derive_describe(comptime T: Type) -> Vec[Item] {
    let n = T.name();
    [ast.item(\"impl \" + n + \" { fn describe(self) -> String { \\\"a \" + n + \"\\\" } }\")]
}

#[derive(Describe)]
struct Widget { id: i64 }

fn main() {
    let w = Widget { id: 7 };
    println(w.describe());
}";
    assert_eq!(karac::run_program(src), vec!["a Widget\n"]);
}

// ── reflection-driven codegen: iterate fields ───────────────────

#[test]
fn derive_iterates_fields() {
    // The derive walks `T.fields()` and emits a structural equality method —
    // the canonical derive use-case (cf. the spec's `derive_eq`).
    // `#[derive(FieldEq)]` → `derive_field_eq` (multi-word snake lookup too).
    let src = "
comptime fn derive_field_eq(comptime T: Type) -> Vec[Item] {
    let n = T.name();
    let mut body = \"true\";
    for f in T.fields() {
        body = body + \" and self.\" + f.name + \" == other.\" + f.name;
    }
    [ast.item(\"impl \" + n + \" { fn field_eq(self, other: \" + n + \") -> bool { \" + body + \" } }\")]
}

#[derive(FieldEq)]
struct Point { x: i64, y: i64 }

fn main() {
    let a = Point { x: 1, y: 2 };
    let b = Point { x: 1, y: 2 };
    let c = Point { x: 1, y: 9 };
    println(a.field_eq(b));
    println(a.field_eq(c));
}";
    assert_eq!(karac::run_program(src), vec!["true\n", "false\n"]);
}

// ── reflection-driven codegen: iterate variants (enum) ──────────

#[test]
fn derive_on_enum_uses_variants() {
    // A derive on an enum reads `T.variants()` and bakes the count into a
    // generated method. `f"{c}"` interpolates the comptime-known count into the
    // quoted source as a literal.
    let src = "
comptime fn derive_arity(comptime T: Type) -> Vec[Item] {
    let n = T.name();
    let c = T.variants().len();
    [ast.item(\"impl \" + n + \" { fn arity(self) -> i64 { \" + f\"{c}\" + \" } }\")]
}

#[derive(Arity)]
enum Color { Red, Green, Blue }

fn main() {
    let c = Color.Red;
    println(c.arity());
}";
    assert_eq!(karac::run_program(src), vec!["3\n"]);
}

// ── lookup convention: CamelCase → snake_case ───────────────────

#[test]
fn derive_snake_case_lookup() {
    // `#[derive(PartialEq)]` resolves to `derive_partial_eq`.
    let src = "
comptime fn derive_partial_eq(comptime T: Type) -> Vec[Item] {
    [ast.item(\"impl \" + T.name() + \" { fn peq(self) -> bool { true } }\")]
}

#[derive(PartialEq)]
struct Q { x: i64 }

fn main() {
    let q = Q { x: 1 };
    println(q.peq());
}";
    assert_eq!(karac::run_program(src), vec!["true\n"]);
}

// ── multiple derives + multiple emitted items ───────────────────

#[test]
fn derive_returns_multiple_items() {
    // One derive returns two separate `impl` items; both are spliced after the
    // derive site and both methods are callable.
    let src = "
comptime fn derive_pair(comptime T: Type) -> Vec[Item] {
    let n = T.name();
    [ast.item(\"impl \" + n + \" { fn a(self) -> i64 { 1 } }\"),
     ast.item(\"impl \" + n + \" { fn b(self) -> i64 { 2 } }\")]
}

#[derive(Pair)]
struct Z { x: i64 }

fn main() {
    let z = Z { x: 0 };
    println(z.a());
    println(z.b());
}";
    assert_eq!(karac::run_program(src), vec!["1\n", "2\n"]);
}

#[test]
fn multiple_derives_on_one_type() {
    // Two distinct derives on the same struct each expand independently.
    let src = "
comptime fn derive_one(comptime T: Type) -> Vec[Item] {
    [ast.item(\"impl \" + T.name() + \" { fn one(self) -> i64 { 1 } }\")]
}
comptime fn derive_two(comptime T: Type) -> Vec[Item] {
    [ast.item(\"impl \" + T.name() + \" { fn two(self) -> i64 { 2 } }\")]
}

#[derive(One, Two)]
struct S { x: i64 }

fn main() {
    let s = S { x: 0 };
    println(s.one());
    println(s.two());
}";
    assert_eq!(karac::run_program(src), vec!["1\n", "2\n"]);
}

// ── coexistence with native (non-comptime) derives ──────────────

#[test]
fn unbacked_derive_is_left_alone() {
    // `#[derive(Eq, Tag)]`: `Eq` has no `derive_eq` comptime fn, so it is left
    // to the existing native handling (no error); `Tag` expands. The pass must
    // not choke on the derive name it doesn't own.
    let src = "
comptime fn derive_tag(comptime T: Type) -> Vec[Item] {
    [ast.item(\"impl \" + T.name() + \" { fn tag(self) -> i64 { 0 } }\")]
}

#[derive(Eq, Tag)]
struct P { x: i64 }

fn main() {
    let p = P { x: 1 };
    println(p.tag());
}";
    assert!(comptime_diags(src).is_empty(), "unexpected diagnostics");
    assert_eq!(karac::run_program(src), vec!["0\n"]);
}

// ── diagnostics ─────────────────────────────────────────────────

#[test]
fn derive_compiler_error_surfaces() {
    // A derive can validate its target with `compiler.error` (composing
    // substrate 2 reflection + substrate 3 diagnostics).
    let src = "
comptime fn derive_check(comptime T: Type) -> Vec[Item] {
    if T.fields().len() == 0 { compiler.error(\"derive_check needs fields\"); }
    [ast.item(\"impl \" + T.name() + \" { fn ok(self) -> bool { true } }\")]
}

#[derive(Check)]
struct Empty {}

fn main() {}";
    let diags = comptime_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR") && d.contains("derive_check needs fields")),
        "expected the user error; got: {diags:?}"
    );
}

#[test]
fn derive_must_return_vec_item() {
    // A `derive_*` that returns something other than a list of items is a
    // comptime error.
    let src = "
comptime fn derive_bad(comptime T: Type) -> i64 { 42 }

#[derive(Bad)]
struct S { x: i64 }

fn main() {}";
    let diags = comptime_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR") && d.contains("must return `Vec[Item]`")),
        "expected a Vec[Item] diagnostic; got: {diags:?}"
    );
}

#[test]
fn ast_item_at_runtime_is_error() {
    // `ast.item` is comptime-only — using it from runtime code is rejected.
    let src = "fn main() { let _i = ast.item(\"fn f() -> i64 { 1 }\"); }";
    let errs = typecheck_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("E_COMPTIME_MODULE_AT_RUNTIME")),
        "expected E_COMPTIME_MODULE_AT_RUNTIME; got: {errs:?}"
    );
}

#[test]
fn ast_item_bad_quote_is_error() {
    // A quote that doesn't parse as a single item is a comptime error.
    let src = "
comptime fn derive_oops(comptime T: Type) -> Vec[Item] {
    [ast.item(\"this is not an item\")]
}

#[derive(Oops)]
struct S { x: i64 }

fn main() {}";
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("ast.item")),
        "expected an ast.item parse diagnostic; got: {diags:?}"
    );
}

// ── no false positives ──────────────────────────────────────────

#[test]
fn no_derive_fns_no_pass() {
    // A program with derives but no `derive_*` comptime fns runs untouched.
    let src = "
struct Point { x: i64, y: i64 }
fn main() { println(42); }";
    assert!(comptime_diags(src).is_empty());
    assert_eq!(karac::run_program(src), vec!["42\n"]);
}
