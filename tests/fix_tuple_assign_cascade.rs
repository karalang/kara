//! B-2026-11-01-11: `(x, y) = (y, x)` on two `String` bindings drew the
//! right E_ASSIGN_TARGET_NOT_PLACE (drop the parentheses) and four cascade
//! E0500 move errors, one per operand and one per later read, each with a
//! `.clone()` fix. `karac fix` applied the clones instead of the place
//! error's edit and, run again, left `x.clone(), y.clone() = ...`, which no
//! fix repairs. The ownership errors inside a refused assignment are now
//! dropped, so the place error is the only one and its fix converges.

use std::process::Command;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-tupcascade-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
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

const STRING_SWAP: &str = "fn main() {\n    let mut x = \"x\".to_string();\n    \
                           let mut y = \"y\".to_string();\n    (x, y) = (y, x);\n    \
                           println(f\"{x} {y}\");\n}\n";

#[test]
fn parenthesized_string_swap_reports_only_the_place_error() {
    let dir = scratch("check");
    std::fs::write(dir.join("s.kara"), STRING_SWAP).unwrap();
    let (ok, text) = karac(&dir, &["check", "s.kara"]);
    assert!(!ok, "check must refuse the parenthesized swap:\n{text}");
    assert!(
        text.contains("drop the parentheses"),
        "the place error must be reported:\n{text}"
    );
    assert!(
        !text.contains("moved here"),
        "no cascade move error may be reported:\n{text}"
    );
    assert!(text.contains("1 error(s) found."), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fix_of_parenthesized_string_swap_applies_the_place_fix_and_runs() {
    let dir = scratch("fix");
    std::fs::write(dir.join("s.kara"), STRING_SWAP).unwrap();
    let (ok, text) = karac(&dir, &["fix", "s.kara"]);
    assert!(ok, "fix failed:\n{text}");
    let got = std::fs::read_to_string(dir.join("s.kara")).unwrap();
    assert!(got.contains("    x, y = y, x;\n"), "{got}");
    assert!(!got.contains("clone"), "no clone may be inserted:\n{got}");
    let (ok, text) = karac(&dir, &["check", "s.kara"]);
    assert!(ok, "the fixed program must check clean:\n{text}");
    let (ok, text) = karac(&dir, &["run", "s.kara"]);
    assert!(ok, "the fixed program must run:\n{text}");
    assert_eq!(text, "y x\n");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A move error outside the refused statement is still reported: only the
/// cascade inside it is dropped.
#[test]
fn a_move_error_outside_a_refused_assignment_is_still_reported() {
    let dir = scratch("outside");
    std::fs::write(
        dir.join("s.kara"),
        "fn take(s: own String) {}\nfn main() {\n    let mut x = \"x\".to_string();\n    \
         let mut y = \"y\".to_string();\n    let z = \"z\".to_string();\n    \
         take(z);\n    take(z);\n    (x, y) = (y, x);\n    println(f\"{x} {y}\");\n}\n",
    )
    .unwrap();
    let (ok, text) = karac(&dir, &["check", "s.kara"]);
    assert!(!ok, "{text}");
    assert!(text.contains("drop the parentheses"), "{text}");
    assert!(text.contains("value 'z' moved"), "{text}");
    assert!(!text.contains("value 'x' moved"), "{text}");
    assert!(!text.contains("value 'y' moved"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
