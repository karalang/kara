//! A `let` that moves a non-`Copy` element out of an index gets the
//! `.clone()` fix, or the `ref` borrow when the binding is only read
//! (B-2026-10-24-18). The borrow was offered for an element of a
//! TEMPORARY too, `let undo = vec!["LR", "RL"][k];`, and the temporary
//! drops at the end of the `let`, so `karac fix` wrote an E0516 it then
//! had no fix for (B-2026-11-03-23, kata 1041's differential). An element
//! of a temporary now keeps the `.clone()` fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `karac check` refuses `src`; `karac fix` repairs it so that it checks
/// clean, contains `want` and prints `out` under `__mir-run`.
fn fixed_to(tag: &str, src: &str, want: &str, out: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-index-let-temporary-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let before = karac(&["check"], &path);
    assert!(!before.status.success(), "{tag}: check accepted the move");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let after = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        after.status.success(),
        "{tag}: still refused after the fix:\n{}\n{text}",
        String::from_utf8_lossy(&after.stdout)
    );
    assert!(text.contains(want), "{tag}: {text}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

#[test]
fn an_element_of_a_temporary_vec_is_fixed_by_a_clone() {
    fixed_to(
        "vec-literal",
        "fn main() {\n    let k = 1;\n    let undo = vec![\"LR\", \"RL\", \"LLLL\"][k];\n    println(f\"{undo} {undo.len()}\");\n}\n",
        // The deprecated `vec!` goes too (design.md § Collection literals).
        "let undo = [\"LR\", \"RL\", \"LLLL\"][k].clone();",
        "RL 2\n",
    );
}

#[test]
fn an_element_of_a_call_result_is_fixed_by_a_clone() {
    fixed_to(
        "call",
        "fn words() -> Vec[String] {\n    vec![\"a\".to_string(), \"bc\".to_string()]\n}\n\nfn main() {\n    let w = words()[1];\n    println(f\"{w}\");\n}\n",
        "let w = words()[1].clone();",
        "bc\n",
    );
}

#[test]
fn an_element_of_a_binding_is_still_borrowed() {
    fixed_to(
        "binding",
        "fn main() {\n    let names = vec![\"LR\", \"RL\"];\n    let k = 1;\n    let undo = names[k];\n    println(f\"{undo}\");\n}\n",
        "let undo = ref names[k];",
        "RL\n",
    );
}
