//! B-2026-11-19-122: an edit that adds a resolve error in one body kept the
//! typecheck warnings of the unchanged bodies, which a whole check does not
//! report (the typecheck never runs past a resolve error). A check whose
//! first failing phase moves is now a whole check, both ways.

use karac::{CheckKind, DocumentChecker, PlaygroundDiagnostic};

const BASE: &str = "fn rp(seed: i64) -> Vec[Vec[i64]] {
    let mut pairs: Vec[Vec[i64]] = Vec.new();
    pairs.push(vec![seed, 2]);
    pairs
}
fn show(p: Vec[Vec[i64]]) { println(p.len()); }
fn main() {
    show(rp(1));
    show(rp(1));
}
";

fn keys(d: &[PlaygroundDiagnostic]) -> Vec<String> {
    let mut v: Vec<String> = d.iter().map(|d| format!("{d:?}")).collect();
    v.sort();
    v
}

#[test]
fn incremental_resolve_error_drops_the_unchanged_bodies_warnings() {
    let edited = BASE.replacen("    show(r", "    showx(r", 1);
    let mut c = DocumentChecker::new();
    let base = c.full(BASE);
    assert!(
        base.iter().any(|d| d.severity == "warning"),
        "the base has the deprecated `vec!` warning: {base:#?}"
    );
    let (inc, _) = c.check_reporting(&edited);
    let full = karac::check_source(&edited);
    assert!(full.iter().all(|d| d.severity == "error"), "{full:#?}");
    assert_eq!(keys(&inc), keys(&full));
    // And back: the warnings return.
    let (back, kind) = c.check_reporting(BASE);
    assert_eq!(keys(&back), keys(&karac::check_source(BASE)));
    assert!(!matches!(kind, CheckKind::Unchanged));
}

/// Any error holds back the `unused_mut` lint (W0301) over the whole
/// program: the corpus sweep's 22 misses were this, a type error added in
/// one body leaving another body's W0301 in place.
#[test]
fn incremental_first_error_drops_the_unchanged_bodies_unused_mut() {
    let base = "fn quiet() -> i64 {
    let mut n = 1;
    n
}
fn main() {
    println(quiet());
}
";
    let edited = base.replacen(
        "    println(quiet());",
        "    let bad: i64 = \"x\";\n    println(quiet());",
        1,
    );
    let mut c = DocumentChecker::new();
    let first = c.full(base);
    assert!(
        first.iter().any(|d| d.code.as_deref() == Some("W0301")),
        "{first:#?}"
    );
    let (inc, _) = c.check_reporting(&edited);
    assert_eq!(keys(&inc), keys(&karac::check_source(&edited)));
    let (back, _) = c.check_reporting(base);
    assert_eq!(keys(&back), keys(&karac::check_source(base)));
}
