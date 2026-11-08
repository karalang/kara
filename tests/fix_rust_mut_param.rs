//! B-2026-11-08-11 — Rust's `mut arr: Vec<i32>` parameter in a LeetCode
//! template (`impl Solution`, `Vec<i32>`, `::` paths) is an owned copy the
//! function may change: `karac fix` writes `arr: own Vec[i32]` and opens the
//! body with `let mut arr = arr;` (the edit landed for katas 2007 and 2008
//! in cdb6382000), together with the template's other Rust spellings, and
//! the fixed program runs. Reported once.

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
    let dir =
        std::env::temp_dir().join(format!("karac-rust-mut-param-{}-{tag}", std::process::id()));
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
fn a_rust_mut_parameter_becomes_own_with_a_let_mut() {
    check_then_fix(
        "solution",
        "struct Solution {}\nimpl Solution {\n    pub fn f(mut arr: Vec<i32>) -> bool {\n        arr.sort();\n        arr[0] < 3\n    }\n}\nfn main() { println(Solution::f([3, 1])); }\n",
        "Rust's `mut arr: T` is an owned parameter the body changes",
        "struct Solution {}\nimpl Solution {\n    pub fn f(arr: own Vec[i32]) -> bool {\n        let mut arr = arr;\n        arr.sort();\n        arr[0] < 3\n    }\n}\nfn main() { println(Solution.f([3, 1])); }\n",
    );
}

#[test]
fn a_mut_parameter_is_reported_once() {
    let path = file(
        "once",
        "fn f(mut a: Vec[i64]) -> i64 {\n    a.push(1);\n    a.len()\n}\nfn main() { println(f([1])); }\n",
    );
    let (_, out) = karac(&["check", path.to_str().unwrap()]);
    assert_eq!(
        out.matches("a parameter's mode is written in its type")
            .count(),
        1,
        "{out}"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
