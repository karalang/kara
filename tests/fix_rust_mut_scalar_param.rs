//! Kata 2169: Rust's `mut num1: i32` parameter is a copy the body changes.
//! `karac fix` wrote `num1: own i32`, as for a `Vec`; a scalar is copied in,
//! so `own` says nothing there and the fix now writes `num1: i32` with the
//! same `let mut num1 = num1;` at the body's start.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn kata_2169_a_rust_mut_scalar_parameter_drops_mut_without_own() {
    let dir = std::env::temp_dir().join(format!("karac-mut-scalar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "fn ops(mut a: i32, mut b: i32, mut v: Vec[i32]) -> i32 {\n    a -= b;\n    b += 1;\n    v.push(a);\n    v[0] + b\n}\nfn main() { println(ops(5, 2, [])); }\n",
    )
    .unwrap();
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains("Rust's `mut a: T` is a copy the body changes"),
        "{out}"
    );
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("fn ops(a: i32, b: i32, v: own Vec[i32]) -> i32 {\n    let mut a = a;\n    let mut b = b;\n    let mut v = v;"),
        "{after}"
    );
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "the fixed program checks:\n{out}");
    let (_, out) = karac(&["__mir-run", p]);
    assert_eq!(out.trim_end(), "6", "{after}");
    let _ = std::fs::remove_dir_all(&dir);
}
