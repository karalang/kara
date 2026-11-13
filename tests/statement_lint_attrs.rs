//! Lint-level attributes on a statement or a block statement
//! (design.md § Lint Level Attributes; the remainder of B-2026-11-05-17
//! item 10). `unused_mut` honours `#[allow]`, `#[deny]` and `#[expect]` on
//! the `let`, innermost first, as `deprecated` does (B-2026-11-13-26); a
//! statement's `#[expect(unfulfilled_lint_expectation)]` and unknown lint
//! names are reported as they are on an item; and an attributed block
//! statement needs no `;` (B-2026-11-13-27).

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn check(src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-stmt-lints-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// (succeeded, stderr)
fn diag(src: &str) -> (bool, String) {
    let out = check(src);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn statement_allow_silences_unused_mut() {
    let (ok, err) =
        diag("fn main() {\n    #[allow(unused_mut)]\n    let mut x = 1;\n    println(x);\n}\n");
    assert!(ok, "{err}");
    assert!(!err.contains("unused_mut"), "{err}");
}

#[test]
fn statement_deny_makes_unused_mut_an_error() {
    let (ok, err) =
        diag("fn main() {\n    #[deny(unused_mut)]\n    let mut x = 1;\n    println(x);\n}\n");
    assert!(!ok, "{err}");
    assert!(err.contains("error[unused_mut]"), "{err}");
}

#[test]
fn statement_expect_is_fulfilled_by_unused_mut() {
    let (ok, err) =
        diag("fn main() {\n    #[expect(unused_mut)]\n    let mut x = 1;\n    println(x);\n}\n");
    assert!(ok, "{err}");
    assert!(!err.contains("unused_mut"), "{err}");
    assert!(!err.contains("unfulfilled"), "{err}");
}

#[test]
fn statement_expect_unused_mut_on_a_written_binding_is_unfulfilled() {
    let (_, err) = diag(
        "fn main() {\n    #[expect(unused_mut)]\n    let mut x = 1;\n    x = x + 1;\n    println(x);\n}\n",
    );
    assert!(err.contains("unfulfilled_lint_expectation"), "{err}");
}

#[test]
fn statement_allow_beats_a_function_deny() {
    let (ok, err) = diag(
        "#[deny(unused_mut)]\nfn main() {\n    #[allow(unused_mut)]\n    let mut x = 1;\n    let mut y = 2;\n    println(x + y);\n}\n",
    );
    assert!(!ok, "{err}");
    assert!(err.contains("`y` is never written"), "{err}");
    assert!(!err.contains("`x` is never written"), "{err}");
}

#[test]
fn statement_expect_on_unfulfilled_is_refused() {
    let (ok, err) = diag(
        "fn main() {\n    #[expect(unfulfilled_lint_expectation)]\n    let x = 1;\n    println(x);\n}\n",
    );
    assert!(!ok, "{err}");
    assert!(err.contains("E_EXPECT_ON_UNFULFILLED"), "{err}");
}

#[test]
fn statement_unknown_lint_is_reported() {
    let (ok, err) =
        diag("fn main() {\n    #[allow(nope_lint)]\n    let x = 1;\n    println(x);\n}\n");
    assert!(ok, "{err}");
    assert!(err.contains("unknown lint name `nope_lint`"), "{err}");
}

#[test]
fn attributed_block_statement_needs_no_semicolon() {
    let (ok, err) = diag(
        "fn main() {\n    #[allow(unused_mut)]\n    {\n        let mut a = 1;\n        println(a);\n    }\n    println(2);\n}\n",
    );
    assert!(ok, "{err}");
    assert!(!err.contains("unused_mut"), "{err}");
    let (ok, err) = diag(
        "fn main() {\n    let c = true;\n    #[allow(unused_mut)]\n    if c {\n        println(1);\n    }\n    println(2);\n}\n",
    );
    assert!(ok, "{err}");
}
