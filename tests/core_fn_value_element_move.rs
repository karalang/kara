//! Moving a function value out of a collection element is refused in the
//! element's words (B-2026-10-22-43). A value of a written function type is
//! move-only (core-semantics §1.1), and check used to read it as `Copy`, so
//! the move reached the MIR borrow check, whose E0517 named the temporary
//! the index lowers to: `move of (*_7) through a shared reference`, later
//! `move of a temporary value through a shared reference`.

use std::process::Command;

/// `karac check` on `src`: its error lines, empty when it passes.
fn check(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-fn-value-element-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    let errors: Vec<&str> = err.lines().filter(|l| l.starts_with("error")).collect();
    assert_eq!(out.status.success(), errors.is_empty(), "{err}");
    errors.join("\n")
}

#[test]
fn core_fn_value_moved_out_of_an_element_names_the_element() {
    let src = "fn doubler(n: i64) -> i64 { n * 2 }\n\
fn main() {\n    let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n    v.push(doubler);\n    let g = v[0];\n    println(f\"{g(21)}\");\n}\n";
    let err = check("let", src);
    assert!(
        err.contains(
            "main.kara:5:13: cannot move a non-`Copy` value out of a collection element: the collection still owns it"
        ),
        "{err}"
    );
    assert!(!err.contains("temporary") && !err.contains("(*_"), "{err}");
}

#[test]
fn core_fn_value_called_or_borrowed_in_an_element_is_not_a_move() {
    let src = "fn doubler(n: i64) -> i64 { n * 2 }\n\
fn main() {\n    let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n    v.push(doubler);\n    println(f\"{v[0](21)}\");\n    let g = v.get(0);\n    println(f\"{v.len()}\");\n}\n";
    assert_eq!(check("call", src), "");
}
