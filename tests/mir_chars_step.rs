//! `StrChars.next` steps over a text a char at a time (B-2026-11-08-6).
//! `char_at_byte` checks and narrows the char on each of its paths and
//! joins them in one `i32` phi, so the width test that follows is
//! answered on the ASCII edge rather than run again on every char; the
//! end test is unsigned, the same test as the decode's bound when `back`
//! is the length; and `pos` steps by the char's width with no overflow
//! check, since a char ends inside its text. Kata 467's bench went from
//! 3.54G to 3.04G instructions (Rust 2.73G). The pins: text of 1- to
//! 4-byte chars walks the same on both backends, front to back, back to
//! front and through a view, and the IR of `StrChars.next` has the phi
//! and no checked add.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = r#"fn widths(s: ref String) -> String {
    let mut out = String.new();
    for c in s.chars() {
        out.push_str(f"{c.len_utf8()}");
    }
    out
}
fn main() {
    let s = "aé€😀b".to_string();
    let fwd: Vec[char] = s.chars().collect();
    let back: Vec[char] = s.chars().rev().collect();
    let view = s.as_str();
    let mut n = 0;
    for c in view.chars() {
        if c != 'b' {
            n += 1;
        }
    }
    println(f"{widths(s)} {fwd.len()} {back[0]}{back[4]} {n}");
}
"#;

const EXPECTED: &str = "12341 5 ba 4\n";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-chars-step-{}", std::process::id()));
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

#[test]
fn a_char_walk_tests_ascii_once_and_steps_unchecked() {
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
    let ir = function(&ll, "StrChars.next");
    assert!(!ir.is_empty(), "no StrChars.next in the IR");
    let text = ir.join("\n");
    assert!(text.contains("%ch = phi i32"), "{text}");
    assert!(!text.contains("with.overflow"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}
