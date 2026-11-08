//! Kata 2018's fold arm: `chain` joining an iterator of `ref T` to one of `T`.
//!
//! - `line.iter().chain(['#'].into_iter())` ties the argument's items
//!   (`char`) to the receiver's (`ref char`) through `chain`'s bound
//!   `J: Iterator[Item = Self.Item]`. The refusal named `.cloned()` in its
//!   message but offered no edit; the fix now appends `.cloned()` to the
//!   side that yields `ref T`: the receiver, or the call's one argument.

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
    std::env::temp_dir().join(format!("karac-kata-2018-{name}-{}", std::process::id()))
}

#[test]
fn kata_2018_chain_of_a_view_and_owned_items_clones_the_receiver() {
    let src = "fn main() {\n    let line: Vec[char] = ['a', 'b'];\n    let n = line.iter().chain(['#'].into_iter()).filter(|c| c != '#').count();\n    println(n);\n}";
    let (first, text, clean, out) = mend(&scratch("clones_the_receiver"), src);
    assert!(first.contains("`.cloned()` turns an iterator"), "{first}");
    assert!(
        text.contains("line.iter().cloned().chain(['#'].into_iter())"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "2\n");
}

#[test]
fn kata_2018_chain_of_owned_and_view_items_clones_the_argument() {
    let src = "fn main() {\n    let tail: Vec[i64] = [3, 4];\n    let total: i64 = [1, 2].into_iter().chain(tail.iter()).sum();\n    println(total);\n}";
    let (first, text, clean, out) = mend(&scratch("clones_the_argument"), src);
    assert!(first.contains("`.cloned()` turns an iterator"), "{first}");
    assert!(
        text.contains("[1, 2].into_iter().chain(tail.iter().cloned())"),
        "{text}"
    );
    assert!(clean, "{text}");
    assert_eq!(out, "10\n");
}
