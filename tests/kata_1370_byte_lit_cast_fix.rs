//! B-2026-11-25-21: W0310's lossless cast on a byte literal, `b'z' as i64 - top`,
//! carries the deletion `karac fix` applies, as it does on an integer
//! literal: a byte literal ends at its closing quote, so deleting ` as i64`
//! keeps it whole, and the subtraction widens it as the warning says. Kata
//! 1370's differential.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The text `karac fix` leaves, its JSON diagnostics afterwards, and its
/// output under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1370-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (_, json) = karac(&dir, &["check", "--output=json", "cell.kara"]);
    let (_, out) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (text, json, out)
}

#[test]
fn kata_1370_byte_literal_cast_is_deleted_by_fix() {
    let (text, json, out) = mend(
        "minus",
        "fn main() {\n    let top: i64 = 100;\n    let by = b'z' as i64 - top;\n    let up = top + b'a' as i64;\n    println(f\"{by} {up}\");\n}\n",
    );
    assert_eq!(
        text,
        "fn main() {\n    let top: i64 = 100;\n    let by = b'z' - top;\n    let up = top + b'a';\n    println(f\"{by} {up}\");\n}\n"
    );
    assert!(json.contains("\"diagnostics\":[]"), "{json}");
    assert_eq!(out, "22 197\n");
}
