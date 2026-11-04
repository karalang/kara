//! design.md § Patterns: there is no `mut name` pattern. A closure parameter
//! written `mut acc` (Rust's `fold(0, |mut acc, x| ..)`) is refused with a
//! machine-applicable fix that drops the `mut` and opens the body with
//! `let mut acc = acc;`, wrapping an expression body in a block
//! (B-2026-11-04-76: "'mut' is a reserved keyword" and a cascade, no fix).

use std::process::Command;

fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-closure-mut-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", f.to_str().unwrap()])
        .output()
        .unwrap();
    let diag = String::from_utf8_lossy(&check.stderr).into_owned();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", f.to_str().unwrap()])
        .output()
        .unwrap();
    let out = std::fs::read_to_string(&f).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp", f.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    (diag, out + &String::from_utf8_lossy(&run.stdout))
}

#[test]
fn a_mut_closure_parameter_is_refused_with_a_rebinding_fix() {
    let (diag, out) = fixed(
        "block",
        "fn main() {\n    let v = [1, 2, 3].to_vec();\n    let s: i64 = v.iter().fold(0, |mut acc, x| { acc += x; acc });\n    println(s);\n}\n",
    );
    assert!(diag.contains("`mut acc` on a closure parameter"), "{diag}");
    assert!(!diag.contains("reserved keyword"), "{diag}");
    assert!(
        out.contains("|acc, x| { let mut acc = acc; acc += x; acc }"),
        "{out}"
    );
    assert!(out.ends_with("6\n"), "{out}");
}

#[test]
fn a_mut_closure_parameter_with_an_expression_body_is_wrapped() {
    let (_, out) = fixed(
        "expr",
        "fn main() {\n    let v = [1, 2, 3].to_vec();\n    let s: i64 = v.iter().fold(0, |mut n, x| n + x);\n    println(s);\n}\n",
    );
    assert!(out.contains("|n, x| { let mut n = n; n + x })"), "{out}");
    assert!(out.ends_with("6\n"), "{out}");
}

#[test]
fn a_mut_nested_in_a_parameter_tuple_is_fixed_in_the_body() {
    let (diag, out) = fixed(
        "nested",
        "fn f(x: i64, (mut a, b): (i64, i64)) -> i64 {\n    a += x;\n    a + b\n}\n\
         fn main() {\n    let v = [1, 2, 3].to_vec();\n    \
         let t = v.iter().map(|x| (x, 1)).fold(0, |acc, (mut a, b)| { a += b; acc + a });\n    \
         println(f\"{f(1, (2, 3))} {t}\");\n}\n",
    );
    assert!(
        diag.contains("`mut a` inside a parameter's pattern"),
        "{diag}"
    );
    assert!(!diag.contains("reserved keyword"), "{diag}");
    assert!(
        out.contains("fn f(x: i64, (a, b): (i64, i64)) -> i64 {\n    let mut a = a;\n    a += x;"),
        "{out}"
    );
    assert!(
        out.contains("|acc, (a, b)| { let mut a = a; a += b; acc + a }"),
        "{out}"
    );
    assert!(out.ends_with("6 9\n"), "{out}");
}
