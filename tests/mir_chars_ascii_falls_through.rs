//! A `chars()` walk's ASCII char falls through (B-2026-11-15-2).
//! `char_at_byte` and `char_before_byte` weight their in-range and ASCII
//! tests toward the in-line path, as `len_utf8`'s test already was, so
//! block placement lays the runtime call out of line. Before, the ASCII
//! path jumped over that call on every char: kata 1573's walk of 10^8
//! chars took 220 ms against Rust's 79 ms with only 1.3x its
//! instructions, and with the weights it takes 86 ms. The pins: text of
//! 1- to 4-byte chars walks the same on both backends, front to back and
//! back to front, and the IR's two tests carry `!prof`.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn ones(s: ref String) -> i64 {
    let v: Vec[i64] = s.chars().enumerate().filter(|(_, c)| c == '1').map(|(i, _)| i).collect();
    v.len()
}
fn main() {
    let s = "1é1€😀1b".to_string();
    let back: Vec[char] = s.chars().rev().collect();
    println(f"{ones(s)} {back.len()} {back[0]}{back[6]}");
}
"#;

const EXPECTED: &str = "3 7 b1\n";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-chars-ascii-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

/// The lines of function `name`'s body (or its located twin's) in the IR.
fn function(ll: &Path, name: &str) -> Vec<String> {
    let ir = std::fs::read_to_string(ll).unwrap();
    let (a, b) = (format!("@kara.{name}("), format!("@kara.{name}.at("));
    ir.lines()
        .skip_while(|l| !(l.starts_with("define") && (l.contains(&a) || l.contains(&b))))
        .take_while(|l| *l != "}")
        .map(str::to_string)
        .collect()
}

/// Whether `ir` branches to `hot` or `cold` with weights.
fn weighted(ir: &[String], hot: &str, cold: &str) -> bool {
    ir.iter().any(|l| {
        l.contains("br i1")
            && l.contains(&format!("label %{hot}"))
            && l.contains(&format!("label %{cold}"))
            && l.contains("!prof")
    })
}

#[test]
fn a_char_walks_ascii_path_is_weighted_to_fall_through() {
    let d = dir();
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        EXPECTED,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })
    .is_none()
    {
        return;
    }
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&ran.stdout), EXPECTED);
    for (f, b) in [("StrChars.next", "ch"), ("StrChars.next_back", "chb")] {
        let ir = function(&ll, f);
        assert!(!ir.is_empty(), "no {f} in the IR");
        let text = ir.join("\n");
        let (look, ascii, slow) = (
            format!("{b}.look"),
            format!("{b}.ascii"),
            format!("{b}.slow"),
        );
        assert!(weighted(&ir, &look, &slow), "{f}:\n{text}");
        assert!(weighted(&ir, &ascii, &slow), "{f}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
