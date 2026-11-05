//! B-2026-11-05-8: `assert`, `assert_eq` and `assert_ne` borrow their
//! operands (design.md §14). B-2026-11-05-22: definite assignment ignores a
//! branch that diverges, and a `loop`'s breaks decide what is assigned after
//! it (design.md §6).

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-abda-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn assertions_leave_their_operands_usable() {
    let (ok, diag) = check(
        "assert_borrows",
        "fn main() {\n    let v: Vec[i64] = [1, 2];\n    let e: Vec[i64] = [1, 2];\n    \
         assert_eq(v, e);\n    assert_ne(v, [3]);\n    println(v.len() + e.len());\n    \
         let s: String = \"a\";\n    assert_ne(s, \"b\");\n    assert(s.len() == 1, s);\n    \
         println(s);\n}\n",
    );
    assert!(ok, "{diag}");
}

#[test]
fn a_diverging_branch_does_not_have_to_assign() {
    for (tag, src) in [
        (
            "if_return",
            "fn f(k: i64) -> i64 {\n    let x: i64;\n    if k > 0 { x = k; } else { return 0; }\n    x\n}\n",
        ),
        (
            "if_panic_then",
            "fn f(k: i64) -> i64 {\n    let x: i64;\n    if k < 0 { panic(\"neg\"); } else { x = k; }\n    x\n}\n",
        ),
        (
            "match_panic_arm",
            "fn f(k: i64) -> i64 {\n    let x: i64;\n    match k {\n        0 => { x = 1; }\n        \
             _ => { panic(\"no\"); }\n    }\n    x\n}\n",
        ),
        (
            "loop_break_in_if",
            "fn f() -> i64 {\n    let z: i64;\n    let mut i = 0;\n    loop {\n        i += 1;\n        \
             if i == 3 {\n            z = i * 10;\n            break;\n        }\n    }\n    z\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, &format!("{src}fn main() {{}}\n"));
        assert!(ok, "{tag}: {diag}");
    }
}

#[test]
fn a_path_that_reaches_the_read_unassigned_is_still_refused() {
    for (tag, src) in [
        (
            "if_falls_through",
            "fn f(k: i64) -> i64 {\n    let x: i64;\n    if k > 0 { x = k; } else { println(0); }\n    x\n}\n",
        ),
        (
            "loop_second_break",
            "fn f() -> i64 {\n    let z: i64;\n    let mut i = 0;\n    loop {\n        i += 1;\n        \
             if i == 3 { z = 1; break; }\n        if i == 5 { break; }\n    }\n    z\n}\n",
        ),
        (
            "loop_break_first",
            "fn f() -> i64 {\n    let z: i64;\n    let mut i = 0;\n    loop {\n        i += 1;\n        \
             if i == 3 { break; }\n        z = 1;\n    }\n    z\n}\n",
        ),
    ] {
        let (ok, diag) = check(tag, &format!("{src}fn main() {{}}\n"));
        assert!(!ok, "{tag}: {diag}");
        assert!(diag.contains("use of uninitialized binding"), "{tag}: {diag}");
    }
}
