//! Kata 1177's Rust arm counts a substring's odd letters with
//! `count_ones()` and holds them against `k` cast `as u32`:
//! `(prefix[r + 1] ^ prefix[l]).count_ones() / 2 <= k`. A bit count is
//! `i64` in Kāra (docs/library/core-types.md, "Bit counts"), so the
//! comparison is `i64 <= u32`. design.md § Mixed-width operands widens
//! the narrower operand of a comparison as of an arithmetic operator when
//! that is lossless, and `u32` widens to `i64`; the arithmetic arms did
//! (B-2026-11-03-12) but the comparison arms still refused every signed
//! and unsigned pair, with no fix. B-2026-11-16-212.

use std::path::Path;
use std::process::{Command, Stdio};

/// The backends each program runs on: the interpreter, and the compiled
/// code where the backend is built.
fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

fn karac(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn cell(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-kata-1177-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

/// The kata's shape: a bit count against a `u32` `k`, in a closure.
#[test]
fn kata_1177_bit_count_compares_with_a_u32() {
    let src = "fn main() {\n    let prefix: Vec[u32] = [0, 1, 3, 7];\n    \
               let qs = [[0, 2, 1], [0, 2, 0]];\n    \
               let v: Vec[bool] = qs.iter().map(|q| {\n        \
               let (l, r, k) = (q[0] as usize, q[1] as usize, q[2] as u32);\n        \
               (prefix[r + 1] ^ prefix[l]).count_ones() / 2 <= k\n    }).collect();\n    \
               println(f\"{v}\");\n}\n";
    let dir = cell("closure", src);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    for mode in modes() {
        let (ok, out) = karac(&dir, &[mode]);
        assert!(ok, "{mode}: {out}");
        assert_eq!(out, "[true, false]\n", "{mode}");
    }
}

/// Every mixed pair where one side holds every value of the other compares
/// as the wider type, on each backend: `u8`, `u16` and `u32` against `i64`,
/// `u16` against `i32`, in either order, with each comparison operator,
/// including values where reading the bits at the wrong signedness would
/// flip the answer (`-1` against `255u8`, `4000000000u32` against `-5`).
#[test]
fn kata_1177_lossless_mixed_sign_comparisons_widen() {
    let src = "fn main() {\n    let a: i64 = -1;\n    let b8: u8 = 255;\n    \
               let b16: u16 = 2;\n    let b32: u32 = 4000000000;\n    let c: i32 = -5;\n    \
               let d: i64 = -5;\n    \
               println(f\"{a < b8} {a == b8} {b8 > a} {a != b16} {c >= b16} {b16 > c}\");\n    \
               println(f\"{b32 > d} {d <= b32} {b32 == 4000000000} {a <= b32}\");\n}\n";
    let dir = cell("pairs", src);
    let (ok, out) = karac(&dir, &["check"]);
    assert!(ok, "{out}");
    for mode in modes() {
        let (ok, out) = karac(&dir, &[mode]);
        assert!(ok, "{mode}: {out}");
        assert_eq!(
            out, "true false true true false true\ntrue true true true\n",
            "{mode}"
        );
    }
}

/// A pair where neither side holds every value of the other still mixes
/// signedness and is refused: `u32` with `i32`, `u64` with `i64`.
#[test]
fn kata_1177_lossy_mixed_sign_comparisons_are_still_refused() {
    for (tag, decl) in [
        ("u32i32", "let a: u32 = 3;\n    let b: i32 = 4;"),
        ("u64i64", "let a: u64 = 3;\n    let b: i64 = 4;"),
    ] {
        let src = format!("fn main() {{\n    {decl}\n    println(f\"{{a < b}}\");\n}}\n");
        let dir = cell(tag, &src);
        let (ok, out) = karac(&dir, &["check"]);
        assert!(!ok, "{tag}: {out}");
        assert!(out.contains("mixing signed and unsigned"), "{tag}: {out}");
    }
}
