//! Kata 1178's Rust arms. One counts letter sets in an unannotated
//! `HashMap` with `*count.entry(m).or_insert(0) += 1` and reads them back
//! with `*count.get(&set).unwrap_or(&0)`, summed into an `i32` answer.
//! The `0` of `unwrap_or` is a literal in a `ref T` slot (design.md §
//! References), so it is a `T`; Kāra typed it `i64` and fixed the map's
//! open counts with it, and the `i32` sum was refused with no fix. Now it
//! joins the open count and a later use fixes both. B-2026-11-16-228.
//!
//! The other builds `let mut res = Vec.with_capacity(n)`, pushes a `cnt`
//! that `cnt += freq.get(..).unwrap_or(0)` over `i32` counts made an `i64`
//! (design.md §8: an operator does not fix a literal binding), and returns
//! `res` as `Vec[i32]`. The refusal named the annotation in prose with no
//! fix; now `karac fix` writes `: Vec[i32]` on `res`, and in the same round
//! `: i32` on `cnt` (B-2026-11-08-20). B-2026-11-16-229.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1178-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// An open map count read back through `get(k).unwrap_or(0)` takes the
/// type its use gives it: an `i32` return, an `i32` sum, a `Vec[i32]`.
#[test]
fn kata_1178_unwrap_or_literal_leaves_map_counts_open() {
    let src = "fn one(words: Vec[i64]) -> i32 {\n    let mut count = Map.new();\n    \
               for w in words.iter() {\n        *count.entry(w).or_insert(0) += 1;\n    }\n    \
               *count.get(2).unwrap_or(0)\n}\n\n\
               fn total(words: Vec[i64]) -> i32 {\n    let mut count = Map.new();\n    \
               for w in words.iter() {\n        *count.entry(w).or_insert(0) += 1;\n    }\n    \
               words.iter().map(|w| *count.get(w).unwrap_or(0)).sum()\n}\n\n\
               fn each(words: Vec[i64]) -> Vec[i32] {\n    let mut count = Map.new();\n    \
               for w in words.iter() {\n        *count.entry(w).or_insert(0) += 1;\n    }\n    \
               words.iter().map(|w| *count.get(w).unwrap_or(0)).collect()\n}\n\n\
               fn main() {\n    let w = [1, 2, 2];\n    \
               println(f\"{one(w)} {total(w)} {each(w)} {one([7])}\");\n}\n";
    let dir = cell("counts", src);
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    for mode in modes() {
        let (ok, out, _) = karac(&dir, &[mode]);
        assert!(ok, "{mode}: {out}");
        assert_eq!(out, "2 5 [1, 2, 2] 0\n", "{mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A count no use fixes stays `i64`; a concrete count is unchanged.
#[test]
fn kata_1178_unwrap_or_literal_default_and_concrete_counts() {
    let src = "fn main() {\n    let mut count = Map.new();\n    \
               *count.entry(1).or_insert(0) += 3000000000;\n    \
               let big = *count.get(1).unwrap_or(0);\n    \
               let mut c: Map[i64, u8] = Map.new();\n    c.insert(1, 200);\n    \
               let small = *c.get(9).unwrap_or(7);\n    \
               println(f\"{big} {small} {*c.get(1).unwrap_or(0)}\");\n}\n";
    let dir = cell("default", src);
    for mode in modes() {
        let (ok, out, _) = karac(&dir, &[mode]);
        assert!(ok, "{mode}: {out}");
        assert_eq!(out, "3000000000 7 200\n", "{mode}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `let mut res = Vec.with_capacity(n)` returned as `Vec[i32]` after
/// pushing an `i64` accumulator: one `karac fix` round annotates both.
#[test]
fn kata_1178_open_vec_refused_at_its_use_gets_its_annotation() {
    let src = "fn f(words: Vec[i32]) -> Vec[i32] {\n    \
               let mut res = Vec.with_capacity(words.len());\n    \
               for w in words.iter() {\n        let mut cnt = 0;\n        cnt += w;\n        \
               res.push(cnt);\n    }\n    res\n}\n\n\
               fn main() {\n    println(f\"{f([1, 2, 2])}\");\n}\n";
    let dir = cell("vec", src);
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("declare it `res: Vec[i32]`"), "{out}");
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{out}");
    assert!(
        fixed.contains("let mut res: Vec[i32] = Vec.with_capacity"),
        "{fixed}"
    );
    assert!(fixed.contains("let mut cnt: i32 = 0;"), "{fixed}");
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out, "[1, 2, 2]\n");
    let _ = std::fs::remove_dir_all(&dir);
}
