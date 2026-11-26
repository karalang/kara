//! E0516 on a temporary argument that the call's result still borrows
//! (`let f = mk("<".to_string());` with `mk` returning a closure over its
//! parameter) carries a machine-applicable fix (B-2026-11-26-80): the
//! temporary is bound by a `let` just before the statement, named after the
//! parameter (or `<param>_arg` when the block already uses that name), and
//! the call passes the binding. A loop or branch header, which runs its
//! expression more than once or not at all, would get none.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "fn mk(p: String) -> Fn(String) -> String { |s| f\"{p}{s}\" }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-temp-arg-hoist-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `karac fix` on the prelude and `src`: the fixed text (prelude dropped)
/// and the fixed program's `__mir-run` stdout, when it checks.
fn fixed(src: &str) -> (String, Option<String>) {
    let (dir, f) = file(src);
    let p = f.to_str().unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", p])
        .output()
        .unwrap();
    let after = std::fs::read_to_string(&f).unwrap();
    let ok = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", p])
        .output()
        .unwrap();
    let out = ok.status.success().then(|| {
        let r = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["__mir-run", p])
            .output()
            .unwrap();
        String::from_utf8_lossy(&r.stdout).into_owned()
    });
    let _ = std::fs::remove_dir_all(&dir);
    (after[PRELUDE.len()..].to_string(), out)
}

#[test]
fn the_temporary_is_bound_before_the_statement() {
    let (src, out) =
        fixed("fn main() {\n    let f = mk(\"<\".to_string());\n    println(f(\"a\"));\n}\n");
    assert_eq!(
        src,
        "fn main() {\n    let p = \"<\".to_string();\n    let f = mk(p);\n    println(f(\"a\"));\n}\n"
    );
    assert_eq!(out.as_deref(), Some("<a\n"));
    // On one line, the binding goes before the statement on that line.
    let (src, out) = fixed("fn main() { let f = mk(\"<\".to_string()); println(f(\"a\")); }\n");
    assert_eq!(
        src,
        "fn main() { let p = \"<\".to_string(); let f = mk(p); println(f(\"a\")); }\n"
    );
    assert_eq!(out.as_deref(), Some("<a\n"));
}

#[test]
fn the_name_is_the_parameter_s_unless_the_block_uses_it() {
    let (src, out) = fixed(
        "fn add(base: i64, pre: String) -> Fn(i64) -> String { |n| f\"{pre}{base + n}\" }\n\
         fn main() {\n    let p = 3;\n    let f = add(1, \"<\".to_string());\n    println(f(p));\n}\n",
    );
    assert!(
        src.contains("    let pre = \"<\".to_string();\n    let f = add(1, pre);\n"),
        "{src}"
    );
    assert_eq!(out.as_deref(), Some("<4\n"));
    let (src, out) = fixed(
        "fn main() {\n    let p = 3;\n    let f = mk(\"<\".to_string());\n    println(f(\"a\"));\n    println(p);\n}\n",
    );
    assert!(
        src.contains("    let p_arg = \"<\".to_string();\n    let f = mk(p_arg);\n"),
        "{src}"
    );
    assert_eq!(out.as_deref(), Some("<a\n3\n"));
}

#[test]
fn a_loop_body_is_fixed() {
    let src = "fn main() {\n    let mut n = 0;\n    for j in 0..2 { let f = mk(j.to_string()); n += f(\"a\").len(); }\n    println(n);\n}\n";
    let (ok, out) = check(src);
    assert!(!ok && out.contains("E0516"), "{out}");
    let (src, out) = fixed(src);
    assert!(
        src.contains("for j in 0..2 { let p = j.to_string(); let f = mk(p);"),
        "{src}"
    );
    assert_eq!(out.as_deref(), Some("4\n"));
}
