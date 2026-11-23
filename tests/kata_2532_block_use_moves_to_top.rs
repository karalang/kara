//! Kata 2532's Rust template wrote `use std::collections::BinaryHeap;`
//! and `use std::cmp::Reverse;` inside a function body, which Rust allows
//! (B-2026-11-23-70). `check` read the `use` as a name, and the `;`-insertion fix
//! made `use; Reverse;`. The fix now moves the line to the top of the
//! file, where the file-level `use` rewrites take it on the next pass.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2532-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const BLOCK_USE: &str = "// a comment above everything

struct Solution {}

impl Solution {
    pub fn top(v: Vec[i64]) -> i64 {
        use std::collections::BinaryHeap;
        use std::cmp::Reverse;
        let mut h = BinaryHeap::new();
        for x in v.iter() {
            h.push(Reverse(*x));
        }
        h.pop().unwrap().0
    }
}

fn main() {
    println(f\"{Solution.top([4, 2, 9])}\");
}
";

#[test]
fn a_use_inside_a_function_moves_to_the_top_of_the_file() {
    let f = program("block-use", BLOCK_USE);
    let (ok, out) = karac(&["check"], &f);
    assert!(!ok);
    assert!(
        out.contains("inside a function is Rust's block-level import"),
        "{out}"
    );
    assert!(!out.contains("undefined name 'use'"), "{out}");
    for _ in 0..3 {
        let _ = karac(&["fix"], &f);
    }
    let fixed = std::fs::read_to_string(&f).unwrap();
    assert!(!fixed.contains("use;"), "{fixed}");
    assert!(!fixed.contains("        use "), "{fixed}");
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check after fix: {out}\n{fixed}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "2\n");
}
