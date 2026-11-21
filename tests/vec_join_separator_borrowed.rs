//! Found writing kata 2788 (Split Strings by Separator).
//!
//! B-2026-11-20-150: `let joined = words.join(sep);` with `sep` an owned
//! `String`, then `joined.split(sep)`, was refused "value 'sep' moved
//! here, used again here". `Slice.join(self, sep: Str)` reads its
//! separator (docs/library/collections.md), and a `Vec` finds every
//! `Slice` method; the ownership pass looked the argument up by the
//! method's name alone, and `LazyFrame.join` owns its first argument, so
//! the name was ambiguous and the argument taken for a move. The receiver's
//! type now decides it.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-vec-join-sep-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const SRC: &str = "fn split_all(words: Vec[String], separator: char) -> Vec[String] {
    let sep = separator.to_string();
    let joined = words.join(sep);
    let mut out: Vec[String] = Vec.new();
    for piece in joined.split(sep) {
        if piece.len() > 0 {
            out.push(piece.to_string());
        }
    }
    out
}

fn main() {
    let words: Vec[String] = [\"a.b\".to_string(), \".c\".to_string()];
    println(f\"{split_all(words, '.')}\");
    let sep = \"-\".to_string();
    let numbers: Vec[i64] = [1, 2, 3];
    let fixed: Array[i64, 2] = [4, 5];
    println(f\"{numbers.join(sep)} {fixed.join(sep)} {numbers[..2].join(sep)} {sep}\");
}
";

#[test]
fn vec_join_reads_its_separator() {
    let out = check(SRC);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(SRC), "[a, b, c]\n1-2-3 4-5 1-2 -\n");
}

#[test]
fn vec_push_still_moves_its_argument() {
    let src = "fn main() {
    let s = \"x\".to_string();
    let mut v: Vec[String] = Vec.new();
    v.push(s);
    println(f\"{s}\");
}
";
    let out = check(src);
    assert!(
        out.contains("value 's' moved here, used again here"),
        "{out}"
    );
}
