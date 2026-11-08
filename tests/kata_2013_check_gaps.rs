//! Kata 2013's Rust-template arm: a map indexed by a borrowed key.
//!
//! - `self.cnt[&(px, py)]` is Rust's index of a `HashMap` by a borrowed
//!   key. The `&` drew the general fix to `ref (px, py)`, a borrow of a
//!   temporary that is then refused ("only `ref <sequence>[i]` is
//!   supported"); a Kāra index takes its key as it is, so the fix now drops
//!   the `&`.

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
    std::env::temp_dir().join(format!("karac-kata-2013-{name}-{}", std::process::id()))
}

#[test]
fn kata_2013_index_by_a_borrowed_key_drops_the_amp() {
    let src = "fn main() {\n    let mut m: Map[(i64, i64), i64] = Map.new();\n    m.insert((1, 2), 7);\n    let (a, b) = (1, 2);\n    let v = vec![5, 6];\n    println(m[&(a, b)] + v[1]);\n}";
    let (first, text, clean, out) = mend(&scratch("index_by_a_borrowed"), src);
    assert!(first.contains("an index takes its key as it is"), "{first}");
    assert!(text.contains("println(m[(a, b)] + v[1]);"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "13\n");
}
