//! Kata 2198 (B-2026-11-15-39): `for a in 1..3usize` lowered the range at
//! the bare start's default `i64` while the loop variable was `usize`, and
//! the MIR it built was invalid. A bare literal bound now takes the type
//! of the suffixed (or typed) other bound, on either side.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2198-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn kata_2198_bare_start_suffixed_end() {
    let (ok, out) = run(
        "end",
        "fn main() {\n    let cnt: Array[i64, 3] = [0, 1, 2];\n    let mut ans: i64 = 0;\n    for a in 1..3usize {\n        ans += cnt[a];\n    }\n    println(f\"{ans}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "3", "{out}");
}

#[test]
fn kata_2198_bare_start_suffixed_inclusive_end() {
    let (ok, out) = run(
        "incl",
        "fn main() {\n    let cnt: Array[i64, 3] = [0, 1, 2];\n    let mut ans: i64 = 0;\n    for a in 1..=2usize {\n        let s = a + 1;\n        if s % a == 0 {\n            ans += cnt[a];\n        }\n    }\n    println(f\"{ans}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "1", "{out}");
}

#[test]
fn kata_2198_typed_start_bare_end() {
    let (ok, out) = run(
        "start",
        "fn main() {\n    let lo: usize = 1;\n    let mut n: usize = 0;\n    for a in lo..4 {\n        n += a;\n    }\n    println(f\"{n}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "6", "{out}");
}
