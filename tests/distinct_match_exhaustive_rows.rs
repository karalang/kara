//! B-2026-11-18-18: a match covering `Flag(true)` and `Flag(false)` on a
//! distinct `bool` was E0205 non-exhaustive. For matching a distinct type
//! has exactly its base type's values (design.md § Pattern exhaustiveness),
//! so a finite base (bool, an enum) is covered by its constructors.

use std::process::{Command, Output};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-distinct-match-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.args(args).arg(&path);
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn a_distinct_over_a_finite_base_is_covered_by_its_constructors() {
    let src = "#[derive(Copy)]
distinct type Flag = bool;
enum Dir { N, S }
distinct type Way = Dir;
fn f(x: Flag) -> i64 { match x { Flag(true) => 1, Flag(false) => 0 } }
fn g(w: Way) -> i64 { match w { Way(Dir.N) => 1, Way(Dir.S) => 2 } }
fn h(o: Option[Flag]) -> i64 { match o { Some(Flag(true)) => 1, Some(Flag(false)) => 2, None => 3 } }
fn main() { println(f(Flag(true)) + f(Flag(false))); println(g(Way(Dir.S))); println(h(Some(Flag(false)))); }
";
    let check = karac("finite", &["check"], src);
    assert!(check.status.success(), "{check:?}");
    let run = karac("finite", &["__mir-run"], src);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1\n2\n2\n", "{run:?}");
}

#[test]
fn a_missing_distinct_constructor_is_named_with_its_wrapper() {
    let out = karac(
        "missing",
        &["check"],
        "#[derive(Copy)]
distinct type Flag = bool;
fn f(x: Flag) -> i64 { match x { Flag(true) => 1 } }
fn main() { println(f(Flag(true))); }
",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("non-exhaustive match: pattern `Flag(false)` not covered"),
        "{err}"
    );
}

#[test]
fn a_wildcard_after_both_distinct_bools_is_unreachable() {
    let out = karac(
        "unreachable",
        &["check"],
        "#[derive(Copy)]
distinct type Flag = bool;
fn f(x: Flag) -> i64 { match x { Flag(true) => 1, Flag(false) => 0, _ => 9 } }
fn main() { println(f(Flag(true))); }
",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning[unreachable_arm]"), "{err}");
}

#[test]
fn a_distinct_over_an_integer_still_needs_a_wildcard() {
    let out = karac(
        "int",
        &["check"],
        "distinct type Id = i64;
fn f(x: Id) -> i64 { match x { Id(1) => 1, Id(2) => 2 } }
fn main() { println(f(Id(1))); }
",
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error[E0205]"), "{err}");
}
