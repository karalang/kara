//! Kata 2138's Rust template collects a string's letters as a chain written
//! one call a line, `s.chars()\n    .collect::<Vec<char>>()\n    .chunks(k)`.
//! The fix that names the collection before the call,
//! `Vec[char].from_iter(..)`, closed its parentheses where the `.` of
//! `.collect` stood, so the line break before it was left inside them and
//! the `)` sat alone on the next line. The `)` now goes where the receiver
//! ends, and the chain keeps its own line breaks after it.

use std::process::{Command, Stdio};

#[test]
fn kata_2138_from_iter_fix_closes_where_the_receiver_ends() {
    let src = "fn letters(s: String) -> i64 {
    s.chars()
        .collect::<Vec<char>>()
        .len()
}
fn main() {
    println(letters(\"kara\"));
}
";
    let path = std::env::temp_dir().join(format!("karac-k2138-fi-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", file])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains("    Vec[char].from_iter(s.chars())\n        .len()\n"),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "4\n");
}
