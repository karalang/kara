//! `blocks` admits `suspends` (`docs/core-semantics.md` §11.8): wherever a
//! declared effect clause is checked as a ceiling (a public function's `with`,
//! the fixed part of `with E ...`, a trait or method ceiling, a function-typed
//! slot), a declared `blocks` covers a `suspends` effect, and a declared
//! `suspends` alone still refuses a `blocks` one.

use karac::{effectcheck, parse};

fn effect_errors(source: &str) -> Vec<String> {
    let parsed = parse(source);
    assert!(parsed.errors.is_empty(), "parse errors in {source}");
    effectcheck(&parsed.program)
        .errors
        .into_iter()
        .filter(|e| !karac::effectchecker::kind_is_note(&e.kind))
        .map(|e| e.to_string())
        .collect()
}

#[test]
fn blocks_declaration_admits_a_suspends_callee() {
    let errors = effect_errors(
        "pub fn wait_net() -> i64 with suspends { return 1; }
         pub fn outer() -> i64 with blocks { wait_net() }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn blocks_in_the_fixed_part_admits_a_suspends_callee() {
    let errors = effect_errors(
        "pub fn wait_net() -> i64 with suspends { return 1; }
         pub fn run_waiting[with E](f: Fn() with E) with E blocks {
             wait_net();
             f()
         }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn suspends_declaration_admits_a_suspends_callee() {
    let errors = effect_errors(
        "pub fn wait_net() -> i64 with suspends { return 1; }
         pub fn outer() -> i64 with suspends { wait_net() }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn suspends_declaration_refuses_a_blocks_callee() {
    let errors = effect_errors(
        "pub fn read_disk() -> i64 with blocks { return 1; }
         pub fn outer() -> i64 with suspends { read_disk() }",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("performs blocks() but does not declare it"),
        "{errors:?}"
    );
}

#[test]
fn blocks_trait_ceiling_admits_a_suspending_impl() {
    let errors = effect_errors(
        "pub fn wait_net() -> i64 with suspends { return 1; }
         pub trait Fetch with blocks {
             fn fetch(ref self) -> i64;
         }
         pub struct Net {}
         impl Fetch for Net {
             fn fetch(ref self) -> i64 { wait_net() }
         }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn blocks_method_ceiling_admits_a_suspending_default_body() {
    let errors = effect_errors(
        "pub fn wait_net() -> i64 with suspends { return 1; }
         pub trait Fetch {
             fn fetch(ref self) -> i64 with blocks { wait_net() }
         }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn suspends_trait_ceiling_refuses_a_blocking_impl() {
    let errors = effect_errors(
        "pub fn read_disk() -> i64 with blocks { return 1; }
         pub trait Fetch with suspends {
             fn fetch(ref self) -> i64;
         }
         pub struct Disk {}
         impl Fetch for Disk {
             fn fetch(ref self) -> i64 { read_disk() }
         }",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("performs blocks()"), "{errors:?}");
}

#[test]
fn blocks_slot_admits_a_suspending_function() {
    let errors = effect_errors(
        "fn wait_net() with suspends { }
         fn apply(g: Fn() with blocks) with blocks { g(); }
         fn main() { apply(wait_net); }",
    );
    assert!(errors.is_empty(), "{errors:?}");
}
