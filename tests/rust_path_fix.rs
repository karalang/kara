//! Rust's `::` paths had no fix: `Vec::new()`, `i64::MAX`, `Dir::Up =>`
//! and `std::cmp::max(a, b)` each drew only "Expected `;` (or `=>`), found
//! `::`", and `std::mem::swap(..)` at the start of a statement drew a
//! WRONG machine fix, a `;` after `std` (B-2026-11-04-5, kata 1043). Each
//! path is now one diagnostic whose fix writes the Kāra spelling: `.` for
//! `::`, and no module path on a `std::` (or `cmp::` / `mem::`) one, since
//! Kāra's `max`, `min`, `swap` and types are in the prelude.

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
    let path =
        std::env::temp_dir().join(format!("karac-rust-path-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).expect("write");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        check.status.success(),
        "{tag}: check after fix: {}\n{text}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        run.status.success(),
        "{tag}: run: {}\n{text}",
        String::from_utf8_lossy(&run.stderr)
    );
    (text, String::from_utf8_lossy(&run.stdout).into_owned())
}

/// The diagnostics `karac check --output=json` reports on `src`, as
/// (message, replacement text) pairs.
fn diagnostics(tag: &str, src: &str) -> Vec<(String, Option<String>)> {
    let path = std::env::temp_dir().join(format!(
        "karac-rust-path-check-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("check --output=json");
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| {
            (
                d["message"].as_str().unwrap_or("").to_string(),
                d["replacement"]["text"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn rust_path_on_an_associated_call_or_constant_becomes_a_dot_path() {
    let (text, out) = fixed(
        "assoc",
        "fn main() {\n    let mut v: Vec[i64] = Vec::new();\n    v.push(i64::MAX);\n    let s = String::from(\"a\");\n    let o = Option::Some(3);\n    println(f\"{v} {s} {o}\");\n}\n",
    );
    assert!(text.contains("Vec.new()"), "{text}");
    assert!(text.contains("v.push(i64.MAX)"), "{text}");
    assert!(text.contains("String.from(\"a\")"), "{text}");
    assert!(text.contains("Option.Some(3)"), "{text}");
    assert_eq!(out, "[9223372036854775807] a Some(3)\n");
}

#[test]
fn rust_std_path_drops_its_module_segments() {
    let (text, out) = fixed(
        "std",
        "fn main() {\n    let mut x = 1;\n    let mut y = 7;\n    let a = std::cmp::max(x, y);\n    let b = cmp::min(x, y);\n    std::mem::swap(mut x, mut y);\n    let c = std::i64::MAX;\n    println(f\"{a} {b} {x} {y} {c}\");\n}\n",
    );
    assert!(text.contains("let a = max(x, y);"), "{text}");
    assert!(text.contains("let b = min(x, y);"), "{text}");
    assert!(text.contains("    swap(mut x, mut y);"), "{text}");
    assert!(text.contains("let c = i64.MAX;"), "{text}");
    assert_eq!(out, "7 1 7 1 9223372036854775807\n");
}

#[test]
fn rust_path_in_a_match_arm_pattern_becomes_a_dot_path() {
    let (text, out) = fixed(
        "pattern",
        "enum Dir {\n    Up,\n    Down,\n}\n\nfn main() {\n    let d = Dir::Down;\n    let s = match d {\n        Dir::Up => 1,\n        Dir::Down => 2,\n    };\n    println(f\"{s}\");\n}\n",
    );
    assert!(text.contains("let d = Dir.Down;"), "{text}");
    assert!(text.contains("Dir.Up => 1,"), "{text}");
    assert!(text.contains("Dir.Down => 2,"), "{text}");
    assert_eq!(out, "2\n");
}

/// The statement-start case used to draw a WRONG machine fix: `std;` with
/// a `;` inserted after the first segment. Now the one diagnostic is the
/// path's, and nothing else is reported.
#[test]
fn rust_path_at_a_statement_start_draws_no_semicolon_fix() {
    let src = "fn main() {\n    let mut x = 1;\n    let mut y = 2;\n    std::mem::swap(mut x, mut y);\n    println(f\"{x} {y}\");\n}\n";
    let found = diagnostics("stmt", src);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].0.contains("`std::mem::swap` is a Rust path"),
        "{found:?}"
    );
    let (text, out) = fixed("stmt", src);
    assert!(text.contains("    swap(mut x, mut y);"), "{text}");
    assert_eq!(out, "2 1\n");
}
