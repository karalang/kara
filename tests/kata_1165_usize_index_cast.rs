//! Kata 1165's Rust arm fills `let mut pos = [0usize; 26];` with
//! `pos[(b - b'a') as usize] = i;` over a `u8` `b`. `karac fix` turned each
//! index into `pos[(b - b'a') as i64]` and a second round dropped the `as
//! i64`, leaving `pos[(b - b'a')]` (B-2026-11-15-50). An index takes the
//! `i64` a narrower integer widens to by itself, and nothing binds around
//! it, so the whole cast now goes in one round with its parentheses, and so
//! does a lossless `as i64` in an index. A cast inside a larger index, or a
//! `let`'s value, still becomes `as i64`, which the value needs.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
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

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1165-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_1165_usize_index_cast_over_a_u8_goes_in_one_round() {
    let (fixed, out) = one_round(
        "index",
        r#"fn main() {
    let mut pos = [0usize; 26];
    for (i, b) in "bca".bytes().iter().enumerate() {
        pos[(b - b'a') as usize] = i;
    }
    let mut t = 0;
    for b in "cab".bytes() {
        t += pos[(b - b'a') as usize];
    }
    println(f"{pos[0]} {pos[1]} {pos[2]} {t}");
}
"#,
    );
    assert!(fixed.contains("pos[b - b'a'] = i;"), "{fixed}");
    assert!(fixed.contains("t += pos[b - b'a'];"), "{fixed}");
    assert!(fixed.contains("[0; 26]"), "{fixed}");
    assert_eq!(out.trim(), "2 0 1 3");
}

#[test]
fn kata_1165_lossless_index_cast_takes_its_parentheses() {
    let (fixed, out) = one_round(
        "lossless",
        r#"fn main() {
    let to = [7, 8, 9];
    let v: Vec[i64] = "bca".bytes().iter().map(|b| to[(b - b'a') as i64]).collect();
    println(f"{v}");
}
"#,
    );
    assert!(fixed.contains("to[b - b'a']"), "{fixed}");
    assert_eq!(out.trim(), "[8, 9, 7]");
}

#[test]
fn kata_1165_usize_cast_inside_a_larger_index_or_let_becomes_i64() {
    let (fixed, out) = one_round(
        "inner",
        r#"fn main() {
    let mut pos = [0usize; 26];
    for (i, b) in "bca".bytes().iter().enumerate() {
        let k = (b - b'a') as usize;
        pos[k] = i;
    }
    let mut t = 0;
    for b in "cab".bytes() {
        t += pos[(b as usize) % 3];
    }
    println(f"{pos[0]} {pos[1]} {pos[2]} {t}");
}
"#,
    );
    assert!(fixed.contains("let k = (b - b'a') as i64;"), "{fixed}");
    assert!(fixed.contains("pos[(b as i64) % 3]"), "{fixed}");
    assert_eq!(out.trim(), "2 0 1 3");
}

#[test]
fn kata_1165_usize_cast_of_a_whole_i64_let_value_takes_its_parentheses() {
    let (fixed, out) = one_round(
        "let",
        r#"fn main() {
    let word = "cab";
    let x: i64 = (word.len() - 1) as usize;
    println(f"{x}");
}
"#,
    );
    assert!(fixed.contains("let x: i64 = word.len() - 1;"), "{fixed}");
    assert_eq!(out.trim(), "2");
}

#[test]
fn kata_1165_index_cast_keeps_a_parenthesized_operand_of_its_own() {
    // The operand's node starts inside its own leading parenthesis; the fix
    // keeps the text between the cast's `(` and `)`, not the node's span.
    let (fixed, out) = one_round(
        "nested",
        r#"fn main() {
    let d = "0123456789abcdef".bytes();
    let n: u8 = 200;
    let mut out: Vec[u8] = Vec.new();
    out.push(d[((n >> 4) & 15) as i64]);
    out.push(d[(n & 15) as i64]);
    let k = ((n >> 4) & 15) as i64;
    println(f"{out[0] as char}{out[1] as char} {k}");
}
"#,
    );
    assert!(fixed.contains("out.push(d[(n >> 4) & 15]);"), "{fixed}");
    assert!(fixed.contains("out.push(d[n & 15]);"), "{fixed}");
    assert!(fixed.contains("let k = ((n >> 4) & 15) as i64;"), "{fixed}");
    assert_eq!(out.trim(), "c8 12");
}
