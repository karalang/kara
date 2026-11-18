//! B-2026-11-18-40: `Self` as the first segment of a path names the type an
//! `impl` is for, and the implementing type in a trait's default body
//! (design.md § Associated functions, review-thread ruling 2026-11-18).
//! The four forms run in corpus/core (`self_path_*`); these pin the
//! front end around them: no internal error reaches the user, `Self`
//! outside an `impl` or trait is refused, and the formatter keeps `Self`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-self-paths-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn run(tag: &str, src: &str, sub: &str) -> Output {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg(sub).arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const IMPL_CALL: &str = "struct S {}\n\nimpl S {\n    fn g(a: i64) -> i64 {\n        a + 1\n    }\n\n    fn f(a: i64) -> i64 {\n        Self.g(a)\n    }\n}\n\nfn main() {\n    println(f\"{S.f(1)}\");\n}\n";

#[test]
fn self_call_in_an_impl_checks_and_runs_with_no_internal_error() {
    let check = run("impl-check", IMPL_CALL, "check");
    assert!(check.status.success(), "{}", text(&check));
    let out = run("impl-run", IMPL_CALL, "__mir-run");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2\n",
        "{}",
        text(&out)
    );
}

#[test]
fn self_outside_an_impl_or_trait_is_a_front_end_error() {
    let src = "fn f() -> i64 {\n    Self.g()\n}\n\nfn main() {\n    println(f\"{f()}\");\n}\n";
    let check = run("outside", src, "check");
    let t = text(&check);
    assert!(!check.status.success(), "{t}");
    assert!(t.contains("'Self' used outside of impl block"), "{t}");
    assert!(!t.contains("E0516") && !t.contains("not lowered"), "{t}");
}

#[test]
fn self_in_a_pattern_outside_an_impl_is_a_front_end_error() {
    let src = "enum E {\n    A,\n    B(i64),\n}\n\nfn f(e: E) -> i64 {\n    match e {\n        Self.A => 1,\n        Self.B(x) => x,\n    }\n}\n\nfn main() {\n    println(f\"{f(E.B(2))}\");\n}\n";
    let check = run("pattern-outside", src, "check");
    let t = text(&check);
    assert!(!check.status.success(), "{t}");
    assert!(t.contains("'Self'"), "{t}");
    assert!(!t.contains("reserved keyword"), "{t}");
}

#[test]
fn a_trait_default_body_calling_a_missing_function_through_self_is_named() {
    let src = "trait Unit {\n    fn one() -> i64;\n    fn nope() -> i64 {\n        Self.missing()\n    }\n}\n\nstruct P {}\n\nimpl Unit for P {\n    fn one() -> i64 {\n        1\n    }\n}\n\nfn main() {\n    println(f\"{P.nope()}\");\n}\n";
    let check = run("missing", src, "check");
    let t = text(&check);
    assert!(!check.status.success(), "{t}");
    assert!(
        t.contains("no method 'missing' found on `Self` in trait 'Unit'"),
        "{t}"
    );
    assert!(!t.contains("E0516"), "{t}");
}

#[test]
fn the_formatter_keeps_self_as_written() {
    let out = run("fmt", IMPL_CALL, "fmt");
    let (dir, path) = fixture("fmt-file", IMPL_CALL);
    let _ = karac().arg("fmt").arg(&path).output().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        after.contains("Self.g(a)") || printed.contains("Self.g(a)"),
        "{after}\n{printed}"
    );
    assert!(
        !after.contains("S.g(a)") && !printed.contains("S.g(a)"),
        "{after}\n{printed}"
    );
}
