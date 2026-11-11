//! An unknown method in a `let` initializer is the one error there: the
//! call has no type, so nothing is known to borrow from a temporary, and
//! "a reference to a temporary outlives its statement" at the same span
//! was a cascade (B-2026-11-10-48, from porting socks5_proxy). A real
//! temporary in the same shape is still refused.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-unknown-method-cascade-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success(), "{tag} checked clean");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn an_unknown_method_in_a_let_chain_is_its_only_error() {
    let stderr = check(
        "trim-end-matches",
        "fn main() {\n    let text = String.from(\"a\\nb\\n\");\n    \
         let lines: Vec[Str] = text.trim_end_matches('\\n').split(\"\\n\").collect();\n    \
         println(lines.len());\n}\n",
    );
    assert!(
        stderr.contains("no method 'trim_end_matches' on type 'String'"),
        "{stderr}"
    );
    assert!(!stderr.contains("a reference to a temporary"), "{stderr}");
    assert!(stderr.contains("1 error(s) found"), "{stderr}");
}

#[test]
fn a_real_temporary_in_a_let_chain_is_still_refused() {
    let stderr = check(
        "string-from",
        "fn main() {\n    let lines: Vec[Str] = String.from(\"a\\nb\").split(\"\\n\").collect();\n    \
         println(lines.len());\n}\n",
    );
    assert!(
        stderr.contains("a reference to a temporary outlives its statement"),
        "{stderr}"
    );
}
