//! The `return_clause_on_fn` lint (B-2026-10-30-9).
//!
//! In return position a trailing `with` belongs to the function, so
//! `-> dyn T with X` leaves the returned value with every effect. The lint
//! names both spellings; the parenthesized ones say which is meant and are
//! silent, and `karac fmt` keeps their parentheses.

use karac::{format_source, parse, resolve, typecheck};

const SHAPES: &str = "
trait Shape {
    fn area(self) -> i64;
}
struct Sq { s: i64 }
impl Shape for Sq {
    fn area(self) -> i64 { self.s * self.s }
}
";

fn warnings_of(items: &str) -> Vec<String> {
    let source = format!("{SHAPES}\n{items}\nfn main() {{}}\n");
    let parsed = parse(&source);
    assert!(
        parsed.errors.is_empty(),
        "expected a clean parse of {source:?}, got: {:?}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    let resolved = resolve(&parsed.program);
    typecheck(&parsed.program, &resolved)
        .warnings
        .iter()
        .map(|w| w.to_string())
        .filter(|w| w.contains("gives the clause to the function"))
        .collect()
}

#[test]
fn return_clause_on_fn_warns_on_bare_dyn() {
    let w = warnings_of("fn make() -> dyn Shape with reads(Clock) { Sq { s: 2 } }");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].contains("`-> (dyn Shape with reads(Clock))`")
            && w[0].contains("`-> (dyn Shape) with reads(Clock)`"),
        "the warning must name both spellings: {}",
        w[0]
    );
}

#[test]
fn return_clause_on_fn_warns_on_bare_function_type() {
    let w = warnings_of("fn make() -> Fn() -> i64 with reads(Clock) { || 3 }");
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("returned function value"), "{}", w[0]);
}

#[test]
fn return_clause_on_fn_warns_on_trait_method() {
    let w = warnings_of(
        "trait Factory {
            fn build(self) -> dyn Shape with reads(Clock);
        }",
    );
    assert_eq!(w.len(), 1, "{w:?}");
}

#[test]
fn return_clause_on_fn_silent_on_parenthesized_spellings() {
    let w = warnings_of(
        "fn on_fn() -> (dyn Shape) with reads(Clock) { Sq { s: 2 } }
         fn on_value() -> (dyn Shape with reads(Clock)) { Sq { s: 2 } }",
    );
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn return_clause_on_fn_silent_without_a_dyn_or_function_return() {
    let w = warnings_of(
        "fn plain() -> i64 with reads(Clock) { 1 }
         fn none() -> dyn Shape { Sq { s: 2 } }",
    );
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn return_clause_on_fn_allow_silences() {
    let w = warnings_of(
        "#[allow(return_clause_on_fn)]
         fn make() -> dyn Shape with reads(Clock) { Sq { s: 2 } }",
    );
    assert!(w.is_empty(), "{w:?}");
}

#[test]
fn return_clause_on_fn_fmt_keeps_parentheses() {
    let src = "fn on_fn() -> (dyn Shape) with reads(Clock) {\n    Sq {\n        s: 2,\n    }\n}\n\n\
               fn on_value() -> (dyn Shape with reads(Clock)) {\n    Sq {\n        s: 2,\n    }\n}\n";
    let out = format_source(src).expect("formats");
    assert!(
        out.contains("-> (dyn Shape) with reads(Clock)")
            && out.contains("-> (dyn Shape with reads(Clock))"),
        "fmt must keep both parenthesized spellings, got:\n{out}"
    );
}
