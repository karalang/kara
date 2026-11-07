//! Rust spellings kata 2002's arms met, each carried by `karac fix` to Kāra.
//!
//! - `fn f(s: &str)`: the `&T` edit made it `ref Str`, and a `String`
//!   argument does not pass as `ref Str`, so each call site failed with no
//!   fix. `&str` is now one diagnostic whose edit writes `Str`, the view a
//!   `String` argument passes as (design.md §5 Strings).

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

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..4 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-2002-{name}-{}", std::process::id()))
}

#[test]
fn kata_2002_borrowed_str_parameter_becomes_str() {
    let src = "fn width(s: &str) -> i64 {\n    s.len()\n}\n\nfn main() {\n    let a = String.from(\"abc\");\n    println(width(a));\n    println(width(\"hello\"));\n}";
    let (first, text, clean, out) = mend(&scratch("str"), src);
    assert!(
        first.contains("`&str` is Rust's borrowed string"),
        "{first}"
    );
    assert!(text.contains("fn width(s: Str) -> i64"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "3\n5\n");
}

#[test]
fn kata_2002_borrowed_string_keeps_the_ref_edit() {
    let src = "fn width(s: &String) -> i64 {\n    s.len()\n}\n\nfn main() {\n    let a = String.from(\"abcd\");\n    println(width(a));\n}";
    let (first, text, clean, out) = mend(&scratch("string"), src);
    assert!(!first.contains("`&str` is Rust's"), "{first}");
    assert!(
        text.contains("String) -> i64") && !text.contains("s: Str)"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "4\n");
}
