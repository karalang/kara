//! Kata 2021's search arm: a `fold` whose closure returns a tuple holding a
//! borrowed scalar.
//!
//! - `starts.iter().fold((0, 0), |(best, answer), x| .. (here, x) ..)` gives
//!   `x: ref i64` as a part of the `(i64, i64)` accumulator. The tuple
//!   literal's own check accepts it, as `let t: (i64, i64) = (1, x)` does,
//!   and a closure whose tail is a bare `x` was already read as `i64`
//!   (B-2026-08-26-19), but the closure's whole type kept the borrow inside
//!   the tuple and was refused against `Fn(..) -> (i64, i64)`.

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
    std::env::temp_dir().join(format!("karac-kata-2021-{name}-{}", std::process::id()))
}

#[test]
fn kata_2021_fold_returning_a_tuple_with_a_borrowed_scalar_checks() {
    let src = "fn main() {\n    let v: Vec[i64] = [3, 1, 2];\n    let pair = v.iter().fold((0, 0), |(a, b), x| (a + 1, x));\n    let best = v.iter().fold((0, 0), |(n, at), x| if x > n { (x, x * 10) } else { (n, at) });\n    println(f\"{pair.0} {pair.1} {best.0} {best.1}\");\n}";
    let (first, _text, clean, out) = mend(&scratch("fold_tuple_ref"), src);
    assert!(!first.contains("error"), "{first}");
    assert!(clean);
    assert_eq!(out, "3 2 3 30\n");
}

#[test]
fn kata_2021_closure_returning_a_tuple_with_a_borrowed_string_is_still_refused() {
    let src = "fn main() {\n    let v: Vec[String] = [\"a\".to_string()];\n    let pair = v.iter().fold((0, String.new()), |(a, b), x| (a + 1, x));\n    println(pair.0);\n}";
    let (first, _text, _clean, _out) = mend(&scratch("fold_tuple_string"), src);
    assert!(first.contains("error"), "{first}");
}
