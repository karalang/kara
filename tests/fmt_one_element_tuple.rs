//! B-2026-11-05-14: `karac fmt` printed a one-element tuple type `(i64,)` as
//! `(i64)`, which is `i64` in parentheses, so the formatted program no longer
//! typechecked; a one-element tuple pattern `(x,)` lost its comma the same
//! way and became a plain binding. The expression form kept its comma. The
//! formatter normalizes layout, not meaning (design.md §17), so the output
//! must keep every comma, run the same, and format to itself.

use std::process::{Command, Stdio};

const SRC: &str = "fn f() -> (i64,) { (4,) }
fn g(t: (String,)) -> i64 { t.0.len() as i64 }
fn h(p: (i64,)) -> i64 {
    let (x,) = p;
    match p {
        (7,) => 70,
        (y,) => x + y,
    }
}
fn main() {
    let v: Vec[(i64,)] = [(1,), (2,)];
    println(v.len());
    println(f().0);
    println(g((\"ab\".to_string(),)));
    let t: (i64,) = (1,);
    println(t.0);
    println(h((5,)));
    println(h((7,)));
}
";

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn fmt_keeps_the_comma_of_a_one_element_tuple_type_and_pattern() {
    let dir = std::env::temp_dir().join(format!("karac-fmt-one-tuple-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), SRC).unwrap();

    let (ok, before, err) = karac(&dir, &["run", "--interp", "a.kara"]);
    assert!(ok, "{before}{err}");
    assert_eq!(before, "2\n4\n2\n1\n10\n70\n");

    let (ok, once, err) = karac(&dir, &["fmt", "a.kara"]);
    assert!(ok, "{once}{err}");
    for want in [
        "fn f() -> (i64,)",
        "t: (String,)",
        "Vec[(i64,)]",
        "let t: (i64,) = (1,);",
        "let (x,) = p;",
        "(7,) => 70",
    ] {
        assert!(once.contains(want), "missing {want:?} in:\n{once}");
    }
    std::fs::write(dir.join("b.kara"), &once).unwrap();
    let (ok, after, err) = karac(&dir, &["run", "--interp", "b.kara"]);
    assert!(ok, "{after}{err}");
    assert_eq!(after, before);

    let (ok, twice, err) = karac(&dir, &["fmt", "b.kara"]);
    assert!(ok, "{twice}{err}");
    assert_eq!(twice, once, "fmt is not idempotent");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_one_element_tuple_type_is_named_with_its_comma() {
    let dir = std::env::temp_dir().join(format!("karac-one-tuple-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("m.kara"),
        "fn main() {\n    let t: i64 = (1,);\n    println(t);\n}\n",
    )
    .unwrap();
    let (ok, out, err) = karac(&dir, &["check", "m.kara"]);
    assert!(!ok, "{out}{err}");
    let all = format!("{out}{err}");
    assert!(all.contains("'(i64,)'"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}
