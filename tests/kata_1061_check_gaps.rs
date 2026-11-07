//! Spellings kata 1061's arms met, each carried by `karac fix` to a clean
//! check.
//!
//! - `s1.bytes().zip(s2.bytes())`: a `Slice` is not an `Iterator`, and the fix
//!   wrote `.iter()` on the receiver but nothing on the argument, which has
//!   no `into_iter` to suggest. The argument now gets the `.iter()` too.
//! - `find(mut parent, (a - b'a') as i64)`: `lossless_cast` had no fix for a
//!   parenthesized operand, whose node ends before its `)`. The fix now
//!   deletes the `as i64` past the `)` and keeps the parentheses.

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
/// text checks clean and what the last check says, and what it prints
/// under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String, String) {
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
    let (clean, _, last) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, last, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-1061-{name}-{}", std::process::id()))
}

#[test]
fn kata_1061_slice_zip_argument_gets_iter() {
    let src = "fn main() {\n    let s1 = \"abc\";\n    let s2 = \"bcd\";\n    let mut n = 0;\n    for (a, b) in s1.bytes().zip(s2.bytes()) {\n        if a < b {\n            n += 1;\n        }\n    }\n    println(n);\n}";
    let (first, text, clean, last, out) = mend(&scratch("zip"), src);
    assert!(first.contains("iterate it with `.iter()`"), "{first}");
    assert!(
        text.contains("s1.bytes().iter().zip(s2.bytes().iter())"),
        "{text}"
    );
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "3\n");
}

#[test]
fn kata_1061_parenthesized_lossless_cast_is_dropped() {
    let src = "fn index(i: i64) -> i64 {\n    i * 2\n}\n\nfn main() {\n    let c = b'd';\n    let a = index((c - b'a') as i64);\n    let b: i64 = (( c - b'b' )) as i64;\n    println(f\"{a} {b}\");\n}";
    let (first, text, clean, last, out) = mend(&scratch("cast"), src);
    assert!(first.contains("widens to 'i64' implicitly"), "{first}");
    assert!(text.contains("index((c - b'a'));"), "{text}");
    assert!(text.contains("let b: i64 = (( c - b'b' ));"), "{text}");
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "6 2\n");
}

#[test]
fn kata_1061_narrowing_cast_keeps_its_as() {
    let src = "fn main() {\n    let x = 300;\n    let y = (x + 1) as u8;\n    println(y);\n}";
    let (first, text, _, _, _) = mend(&scratch("narrow"), src);
    assert!(!first.contains("unnecessary cast"), "{first}");
    assert!(text.contains("(x + 1) as u8"), "{text}");
}
