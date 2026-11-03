//! `(a, b) = (c, d)` is refused (a parenthesized tuple is a value), and
//! its machine-applicable fix drops the parentheses: `a, b = c, d`. As the
//! body of a braceless `match` arm that edit did not parse, since the arm
//! ends at the first `,`, so `karac fix` refused to write the file
//! (B-2026-11-03-20, kata 1041's `'L' => (dx, dy) = (-dy, dx),`). There
//! the fix now braces the statement: `'L' => { dx, dy = -dy, dx; }`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Runs `karac fix` on `src` and returns the fixed text and what
/// `karac __mir-run` prints from it.
fn fixed(tag: &str, src: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!(
        "karac-multi-assign-arm-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        run.status.success(),
        "{tag}: run: {}\n{text}",
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn multi_assign_fix_braces_a_braceless_arm() {
    let (text, out) = fixed(
        "arm",
        "fn main() {\n    let mut (dx, dy) = (0, 1);\n    for c in \"LLR\".chars() {\n        match c {\n            'L' => (dx, dy) = (-dy, dx),\n            _ => (dx, dy) = (dy, -dx),\n        }\n    }\n    println(f\"{dx} {dy}\");\n}\n",
    );
    assert!(text.contains("'L' => { dx, dy = -dy, dx; },"), "{text}");
    assert!(text.contains("_ => { dx, dy = dy, -dx; },"), "{text}");
    assert_eq!(out, "-1 0\n");
}

#[test]
fn multi_assign_fix_in_a_braced_arm_and_a_statement_drops_only_the_parentheses() {
    let (text, out) = fixed(
        "stmt",
        "fn main() {\n    let mut (a, b) = (1, 2);\n    (a, b) = (b, a);\n    match a {\n        2 => {\n            (a, b) = (b + 10, a);\n        }\n        _ => {}\n    }\n    println(f\"{a} {b}\");\n}\n",
    );
    assert!(text.contains("\n    a, b = b, a;\n"), "{text}");
    assert!(text.contains("\n            a, b = b + 10, a;\n"), "{text}");
    assert_eq!(out, "11 2\n");
}

#[test]
fn a_braceless_arm_assignment_of_one_place_still_checks() {
    let (_, out) = fixed(
        "single",
        "fn main() {\n    let mut f = 0;\n    for c in \"LLR\".chars() {\n        match c {\n            'L' => f = (f + 3) % 4,\n            _ => f = (f + 1) % 4,\n        }\n    }\n    println(f\"{f}\");\n}\n",
    );
    assert_eq!(out, "3\n");
}
