//! Found writing kata 1137's matrix arm (B-2026-11-12-9).
//!
//! `let mut result = [[1, 0, 0], [0, 1, 0], [0, 0, 1]];` passed to an
//! `Array[Array[i64, 3], 3]` parameter is a `Vec[Vec[i64]]` and is refused,
//! as a flat list literal is (B-2026-11-10-47), but the refusal said
//! nothing and offered no fix: only a flat literal's length was recorded,
//! and `result = multiply(result, step)` counted as another value though
//! the `Array` it stores carries the same lengths. Now a literal whose
//! elements are list literals of one shape gets the nested `Array`, and
//! storing an `Array` into the binding keeps the fix.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1137-nested-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const MATRIX: &str = "fn multiply(x: Array[Array[i64, 2], 2], y: Array[Array[i64, 2], 2]) -> Array[Array[i64, 2], 2] {
    let mut z = [[0; 2]; 2];
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..2 {
                z[i][j] += x[i][k] * y[k][j];
            }
        }
    }
    z
}

fn fib(n: i64) -> i64 {
    let mut result = [[1, 0], [0, 1]];
    let mut step = [[1, 1], [1, 0]];
    let mut e = n;
    while e > 0 {
        if e % 2 == 1 {
            result = multiply(result, step);
        }
        e /= 2;
        if e > 0 {
            step = multiply(step, step);
        }
    }
    result[1][0]
}

fn main() {
    println(f\"{fib(10)} {fib(50)}\");
}
";

#[test]
fn a_nested_list_literal_used_as_a_nested_array_is_declared_one() {
    let path = temp("matrix", MATRIX);
    let out = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{stderr}");
    // Each use of each binding is explained once.
    assert_eq!(
        stderr
            .matches("`result` (bound at 14:13) is a `Vec`, because a list literal")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("`step` (bound at 15:13) is a `Vec`, because a list literal")
            .count(),
        3,
        "{stderr}"
    );
    assert!(
        stderr.contains("declare it `result: Array[Array[i64, 2], 2]`"),
        "{stderr}"
    );
    let fixed = karac(&["fix"], &path);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("let mut result: Array[Array[i64, 2], 2] = [[1, 0], [0, 1]];"),
        "{text}"
    );
    assert!(
        text.contains("let mut step: Array[Array[i64, 2], 2] = [[1, 1], [1, 0]];"),
        "{text}"
    );
    let out = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "55 12586269025\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_nested_list_literal_of_another_shape_or_given_a_vec_gets_no_fix() {
    let src = "fn f(a: Array[Array[i64, 2], 2]) -> i64 {
    a[0][0]
}

fn main() {
    let ragged = [[1, 2], [3]];
    println(f(ragged));
    let wide = [[1, 2, 3], [4, 5, 6]];
    println(f(wide));
    let mut grown = [[1, 2], [3, 4]];
    grown = [[5, 6], [7, 8]];
    println(f(grown));
}
";
    let path = temp("edges", src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{stderr}");
    assert!(!stderr.contains("`ragged` (bound"), "{stderr}");
    assert!(!stderr.contains("`wide` (bound"), "{stderr}");
    // A list literal stored into it may have other lengths: said, not fixed.
    assert!(
        stderr.contains("`grown` (bound at 10:13) is a `Vec`"),
        "{stderr}"
    );
    assert!(!stderr.contains("declare it"), "{stderr}");
}
