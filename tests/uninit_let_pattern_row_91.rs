//! Rust's `let (x, y, z);`, assigned in each branch afterwards, is told at
//! once that a destructuring `let` needs a value, with the form to write.
//! It was told to add a type first, and `let (x, y, z): (i32, i32, i32);`
//! then met the destructuring refusal (B-2026-11-18-91).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own, whatever names two tests pass.
static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac check`'s output for `src`, which it must refuse.
fn refusal(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-uninit-let-pattern-{name}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "{name}: accepted");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const BRANCHES: &str = "    if c {\n        x = 1;\n        y = 2;\n        z = 3;\n    } else {\n        x = 4;\n        y = 5;\n        z = 6;\n    }\n    println(x + y + z);\n}\n";

/// With or without the annotation, the one message is the destructuring
/// one, at the pattern, and it writes the pattern into the form to use.
#[test]
fn a_destructuring_let_with_no_value_is_told_so_before_any_annotation() {
    for (name, decl) in [
        ("bare", "let (x, y, z);"),
        ("annotated", "let (x, y, z): (i32, i32, i32);"),
    ] {
        let text = refusal(
            name,
            &format!("fn main() {{\n    let c = true;\n    {decl}\n{BRANCHES}"),
        );
        assert!(
            text.contains(":3:9: uninitialized `let` must bind a single name; destructuring patterns require an initializer"),
            "{name}: {text}"
        );
        assert!(
            text.contains("(`let (x, y, z) = if .. { .. } else { .. };`), or declare each name on its own (`let x: T;`)"),
            "{name}: {text}"
        );
        assert!(
            !text.contains("requires a type annotation"),
            "{name}: {text}"
        );
        assert!(text.contains("1 error(s) found."), "{name}: {text}");
    }
}

/// The form the message names checks and runs.
#[test]
fn the_form_the_destructuring_message_names_checks_and_runs() {
    let out = karac::mir::lower::run_source(
        "fn main() {\n    let c = true;\n    let (x, y, z) = if c { (1, 2, 3) } else { (4, 5, 6) };\n    println(x + y + z);\n}\n",
    )
    .expect("MIR interpreter")
    .output;
    assert_eq!(out, "6\n");
}

/// A single name with no type still gets the annotation message.
#[test]
fn a_single_name_with_no_type_and_no_value_still_asks_for_the_annotation() {
    let text = refusal(
        "single",
        "fn main() {\n    let x;\n    x = 1;\n    println(x);\n}\n",
    );
    assert!(
        text.contains("uninitialized `let` requires a type annotation; write `let x: T;`"),
        "{text}"
    );
}
