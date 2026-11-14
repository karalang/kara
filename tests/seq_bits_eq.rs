//! B-2026-11-13-33: `==` on two `Vec`s or slices of integers, `bool` or
//! `char` compared one element a time, so `t[i..j] != t[k..l]` over
//! `text.bytes()` ran a byte per iteration where `Str` equality calls
//! `memcmp` (kata 1147's byte arm 1.7x Rust's). Such a run is now one
//! `__bits_eq`: lengths, then one `memcmp` of the bytes. The answers are
//! the element loop's on every backend.

use std::process::{Command, Stdio};

const SRC: &str = "fn main() {\n\
    let a: Vec[u8] = vec![1, 2, 3, 4];\n\
    let b: Vec[u8] = vec![1, 2, 3, 4];\n\
    let c: Vec[u8] = vec![1, 2, 3, 5];\n\
    println(f\"{a == b} {a == c} {a != c} {a[0..2] == b[0..2]} {a[1..3] == c[1..3]} {a[0..0] == c[3..3]}\");\n\
    let s = \"abcabc\".to_string();\n\
    let t = s.bytes();\n\
    println(f\"{t[0..3] == t[3..6]} {t[0..3] != t[3..6]} {t[0..2] == t[1..3]}\");\n\
    let w: Vec[i64] = vec![-1, 5, 1 << 40];\n\
    let x: Vec[i64] = vec![-1, 5, 1 << 40];\n\
    let cs: Vec[char] = vec!['a', 'é'];\n\
    let ds: Vec[char] = vec!['a', 'é'];\n\
    let bs = vec![true, false];\n\
    let fs = vec![true, true];\n\
    println(f\"{w == x} {cs == ds} {bs == fs} {w[..2] == x[1..]}\");\n\
    let e1: Vec[i32] = Vec.new();\n\
    let e2: Vec[i32] = Vec.new();\n\
    println(e1 == e2);\n\
}\n\
";

const WANT: &str =
    "true false true true true true\ntrue false false\ntrue true false false\ntrue\n";

fn run(sub: &str, tag: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-bits-eq-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn bitwise_sequence_equality_interpreted() {
    let (out, err) = run("__mir-run", "interp");
    assert_eq!(out, WANT, "{err}");
}

#[test]
fn bitwise_sequence_equality_compiled() {
    if !cfg!(feature = "llvm") {
        return;
    }
    let (out, err) = run("__mir-llvm-run", "llvm");
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, WANT, "{err}");
}
