//! B-2026-11-21-64 — a value that is an `if` over arithmetic with a defaulted
//! literal binding (`let changed = if d == 6 { num + 3 * place } else
//! { num - 3 * place }` with `let mut place = 1`, `num: i32`), refused at
//! an `i32` `max`, got no fix, where the same arithmetic without the `if`
//! is told to declare `place: i32` (B-2026-11-10-30). The `if` is now read
//! through to both tails. LeetCode 1323's brute arm.

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

fn program(changed: &str) -> String {
    format!(
        "fn maximum69(num: i32) -> i32 {{
    let mut best = num;
    let mut place = 1;
    while place <= num {{
        let d = num / place % 10;
        let changed = {changed};
        best = best.max(changed);
        place *= 10;
    }}
    best
}}

fn main() {{
    println(f\"{{maximum69(9669)}}\");
}}
"
    )
}

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-ifcopy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn an_if_over_a_literal_binding_is_fixed_on_the_binding() {
    let path = scratch(
        "fix",
        &program("if d == 6 { num + 3 * place } else { num - 3 * place }"),
    );
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok, "{out}");
    assert!(out.contains("place: i32"), "{out}");
    let _ = karac(&["fix", p]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("let mut place: i32 = 1;"), "{fixed}");
    let (ok, out) = karac(&["run", p]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(ok, "{out}");
    assert_eq!(out, "9969\n");
}

/// An `else if` chain is read through each tail.
#[test]
fn an_else_if_chain_over_a_literal_binding_is_fixed_on_the_binding() {
    let path = scratch(
        "chain",
        &program("if d == 6 { num + 3 * place } else if d == 9 { num - 3 * place } else { num }"),
    );
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(!ok, "{out}");
    assert!(out.contains("place: i32"), "{out}");
}

/// A branch with statements is not a bare tail, and gets no fix.
#[test]
fn an_if_whose_branch_has_statements_is_not_read_through() {
    let path = scratch(
        "stmts",
        &program("if d == 6 { let t = 3 * place; num + t } else { num - 3 * place }"),
    );
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(!ok, "{out}");
    assert!(!out.contains("place: i32"), "{out}");
}
