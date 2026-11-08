//! B-2026-11-08-13 — Rust's `if let Some(&i) = pos[d].last()` copies the
//! value out of the `Option<&T>`. `karac fix` used to delete only the `&`,
//! leaving `Some(i)` a borrow of `pos[d]` that a later `pos[d].pop()`
//! conflicts with. In an `if let` / `while let` over exactly `Some(&p)` the
//! fix also writes `.cloned()` on the scrutinee; any other `&` pattern keeps
//! the plain deletion. The fixed program checks.

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

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-some-amp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// The message `karac check` gives, then the program `karac fix` leaves,
/// which must check.
fn check_then_fix(tag: &str, src: &str, message: &str, fixed: &str) {
    let path = file(tag, src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains(message),
        "{tag}: want `{message}`:\n{out}"
    );
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, fixed, "{tag}: karac fix");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{tag}: the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn some_amp_in_if_let_and_while_let_clones_the_scrutinee() {
    check_then_fix(
        "some-amp",
        "fn main() {\n    let mut pos: Vec[Vec[i64]] = [[1, 2], [3]];\n    let d = 0;\n    if let Some(&i) = pos[d].last() {\n        pos[d].pop();\n        println(i);\n    }\n    let mut st: Vec[i64] = [5, 6];\n    while let Some(&top) = st.last() {\n        st.pop();\n        println(top);\n    }\n    for &x in [7, 8].iter() { println(x); }\n}\n",
        "Kāra writes that copy as `.cloned()` on the value matched",
        "fn main() {\n    let mut pos: Vec[Vec[i64]] = [[1, 2], [3]];\n    let d = 0;\n    if let Some(i) = pos[d].last().cloned() {\n        pos[d].pop();\n        println(i);\n    }\n    let mut st: Vec[i64] = [5, 6];\n    while let Some(top) = st.last().cloned() {\n        st.pop();\n        println(top);\n    }\n    for x in [7, 8] { println(x); }\n}\n",
    );
}
