//! B-2026-11-21-57 (design.md §8 Literals): a tuple of number literals
//! passed for a generic parameter the call returns, a `fold` seed `(0, 0)`
//! or `id((0, 0))`, keeps its numbers open until a use fixes them, as the
//! same tuple bound by a `let` does and as a lone literal seed does. They
//! were settled at `i64` at the call, so a `.0` returned as `i32` was
//! refused as a narrowing. Kata 1288's Rust arm, a `fold((0, 0), ..)` over
//! `Vec<Vec<i32>>` intervals, found it.

use std::process::{Command, Stdio};

fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-tuple-seed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_tuple_fold_seed_takes_its_numbers_from_the_result() {
    let (ok, out, err) = run(
        "fold",
        r#"fn count_and_last(v: Vec[i32]) -> i32 {
    v.iter().fold((0, 0), |(n, last), x| (n + 1, last + x)).0
}

fn bound(v: Vec[i32]) -> i32 {
    let t = v.iter().fold((0, 0), |(n, last), x| (n + 1, last)).0;
    t
}

fn ident() -> i32 {
    let t = id((0, 0));
    t.0
}

fn id[T](x: own T) -> T {
    x
}

fn kept(intervals: Vec[Vec[i32]]) -> i32 {
    intervals
        .iter()
        .fold((0, 0), |(count, end), iv| {
            if iv[1] > end {
                (count + 1, iv[1])
            } else {
                (count, end)
            }
        })
        .0
}

fn main() {
    println(f"{count_and_last([4, 5, 6])} {bound([1, 2])} {ident()} {kept([[1, 4], [2, 3], [3, 6]])}");
}
"#,
    );
    assert!(ok, "refused: {err}");
    assert_eq!(out, "3 2 0 2\n");
}

#[test]
fn a_tuple_fold_seed_still_defaults_when_nothing_fixes_it() {
    let (ok, out, err) = run(
        "default",
        r#"fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let t = v.iter().fold((0, 0), |(a, b), x| (a + 1, b + x));
    let big: i64 = 5000000000;
    println(f"{t.0 + big} {t.1}");
}
"#,
    );
    assert!(ok, "refused: {err}");
    assert_eq!(out, "5000000003 6\n");
}
