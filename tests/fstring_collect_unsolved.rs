//! B-2026-11-18-10 (kata 1616): an unannotated `collect()` in an f-string
//! hole, or handed to `println`, has nothing to fix what it builds, and it
//! reached the MIR builder as E0516 "an expression whose type does not
//! lower (UnsolvedParam("C")) is not lowered yet". It is now the type
//! checker's E0238, as the same `collect()` bound by a bare `let` is.

use std::process::{Command, Stdio};

fn check(body: &str) -> String {
    let src = format!(
        "fn take(v: Vec[char]) {{ println(v.len()); }}\nfn main() {{\n    let b = \"hello\";\n    {body}\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!(
        "karac-fstring-collect-{}-{}",
        std::process::id(),
        body.len()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("f.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "f.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn collect_in_an_fstring_hole_is_a_type_error() {
    let out = check("let s = f\"{b.chars().take(2).collect()}!\";\n    println(s);");
    assert!(out.contains("error[E0238]"), "{out}");
    assert!(out.contains("cannot infer type parameter 'C'"), "{out}");
    assert!(!out.contains("E0516"), "{out}");
}

#[test]
fn collect_handed_to_println_is_a_type_error() {
    let out = check("println(b.chars().collect());");
    assert!(out.contains("error[E0238]"), "{out}");
    assert!(!out.contains("E0516"), "{out}");
}

#[test]
fn collect_fixed_by_a_parameter_still_checks() {
    let out = check("take(b.chars().collect());");
    assert!(!out.contains("error"), "{out}");
    let out =
        check("let x: Option[Vec[char]] = Some(b.chars().collect());\n    println(x.is_some());");
    assert!(!out.contains("error"), "{out}");
}
