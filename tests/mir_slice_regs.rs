//! A slice or `str` value is passed and returned in registers, as the
//! pair `{ptr, i64}`, so a function taking one keeps the by-value
//! convention its other parameters have (B-2026-10-31-40: with the slice
//! in memory, every recursive call of kata 10's matcher copied both of
//! its slices and returned its `bool` through a pointer). The pins: the
//! program prints the same under both backends, the matcher's signature
//! takes `{ ptr, i64 }` and returns its `bool` as a value, and with the
//! registers off (`KARAC_MIR_SLICE_REGS=0`) it takes only pointers, so
//! the pin can fail.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SRC: &str = r#"fn is_match_at(s: Slice[u8], i: i64, p: Slice[u8], j: i64) -> bool {
    let n = s.len();
    let m = p.len();
    if j == m {
        return i == n;
    }
    let first_match: bool = i < n and (p[j] == s[i] or p[j] == b'.');
    if j + 1 < m and p[j + 1] == b'*' {
        return is_match_at(s, i, p, j + 2)
            or (first_match and is_match_at(s, i + 1, p, j));
    }
    first_match and is_match_at(s, i + 1, p, j + 1)
}

fn tail(s: Slice[u8], k: i64) -> Slice[u8] {
    s[k..]
}

fn main() {
    let strs = ["aa", "ab", "aab", "mississippi", "aaa"];
    let pats = ["a*", ".*", "c*a*b", "mis*is*p*.", "ab*a"];
    let mut i = 0;
    while i < 5 {
        let s: String = strs[i].to_string();
        let p: String = pats[i].to_string();
        println(is_match_at(s.bytes(), 0, p.bytes(), 0));
        i = i + 1;
    }
    let w: String = "abcdef".to_string();
    println(tail(w.bytes(), 2).len());
}
"#;

const EXPECTED: &str = "true\ntrue\ntrue\nfalse\nfalse\n4\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-slice-regs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// The `define` line of function `name` in the IR at `ll`.
fn signature(ll: &Path, name: &str) -> String {
    let ir = std::fs::read_to_string(ll).unwrap();
    let head = format!("@kara.{name}(");
    ir.lines()
        .find(|l| l.starts_with("define") && l.contains(&head))
        .unwrap_or_default()
        .to_string()
}

/// The matcher's signature and the program's output, built with `regs`.
fn build(d: &Path, regs: &str) -> Option<(String, String)> {
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .env("KARAC_MIR_SLICE_REGS", regs)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })?;
    let ran = Command::new(&exe).output().unwrap();
    Some((signature(&ll, "is_match_at"), out(&ran)))
}

#[test]
fn a_slice_parameter_is_passed_in_registers() {
    let d = dir("pin");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        out(&interp),
        EXPECTED,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let Some((sig, ran)) = build(&d, "1") else {
        return;
    };
    assert_eq!(ran, EXPECTED);
    assert!(
        sig.starts_with("define internal i8 ") && sig.matches("{ ptr, i64 }").count() == 2,
        "{sig}"
    );
    let Some((off, ran)) = build(&d, "0") else {
        return;
    };
    assert_eq!(ran, EXPECTED);
    assert!(
        off.starts_with("define internal void ") && !off.contains("{ ptr, i64 }"),
        "{off}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
