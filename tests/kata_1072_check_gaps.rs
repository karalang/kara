//! Kata 1072's trie arm was first written with Rust's array type as a
//! generic argument, `let mut next: Vec[[i64; 2]] = [[-1, -1]];`. On its
//! own `[T; N]` is reported with a machine fix to `Array[T, N]`, but inside
//! a generic argument list a `[` was taken for a shape literal
//! (`Tensor[f64, [3, 4]]`), whose parser stopped at the `;` with "Expected
//! `]`, found `;`", and `karac fix` inserted a second `;`. A bracket holding
//! a `;` at its own depth is now read as a type there.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The first check's diagnostics, the text one `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn one_round(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1072-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

#[test]
fn kata_1072_rust_array_type_as_a_generic_argument_fixes_in_one_round() {
    let src = "fn ends(rows: Vec[[i64; 2]]) -> i64 {\n    let mut next: Vec[[i64; 2]] = [[-1, -1]];\n    for r in rows.iter() {\n        next.push([r[0] + 1, r[1] + 1]);\n    }\n    next[next.len() - 1][1]\n}\n\nfn main() {\n    let named: Map[String, [i64; 3]] = Map.new();\n    println(f\"{ends([[1, 2], [3, 4]])} {named.len()}\");\n}";
    let (first, text, clean, out) = one_round("generic", src);
    assert_eq!(first.matches("is Rust's array type").count(), 3, "{first}");
    assert!(
        !first.contains("Expected `]`") && !first.contains("Expected expression"),
        "{first}"
    );
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(
        text.contains("fn ends(rows: Vec[Array[i64, 2]]) -> i64 {")
            && text.contains("let mut next: Vec[Array[i64, 2]] = [[-1, -1]];")
            && text.contains("let named: Map[String, Array[i64, 3]] = Map.new();"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "5 0", "{text}");
}
