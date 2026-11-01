//! `unsafe { xs.get_unchecked(i) }` emits no bounds check on mir-llvm
//! (B-2026-10-31-45): design.md § Bounds checks and get_unchecked makes it
//! the opt-out, with the caller ensuring `i < len`. It went through the
//! checked element path, so kata 28's unchecked KMP ran as fast as the
//! checked one. The pins: it reads the right elements under both
//! backends, its function's IR has no `icmp ult` against the length, and
//! the same loop through `xs[i]` keeps its check.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SRC: &str = r#"fn total_unchecked(xs: ref Vec[i64]) -> i64 {
    let mut s = 0;
    let mut i = 0;
    let n = xs.len();
    while i < n {
        s = s + unsafe { xs.get_unchecked(i) };
        i = i + 2;
    }
    s
}
fn total_checked(xs: ref Vec[i64], step: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < 6 {
        s = s + xs[i];
        i = i + step;
    }
    s
}
fn main() {
    let xs: Vec[i64] = [1, 2, 3, 4, 5, 6, 7].to_vec();
    println(f"{total_unchecked(xs)}");
    println(f"{total_checked(xs, 2)}");
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-get-unchecked-{name}-{}", std::process::id()));
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

/// The lines of function `name`'s body in the IR at `ll`.
fn function(ll: &Path, name: &str) -> Vec<String> {
    let ir = std::fs::read_to_string(ll).unwrap();
    let head = format!("@kara.{name}(");
    ir.lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains(&head)))
        .take_while(|l| *l != "}")
        .map(str::to_string)
        .collect()
}

#[test]
fn get_unchecked_emits_no_bounds_check() {
    let d = dir("pin");
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        out(&interp),
        "16\n9\n",
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let exe = d.join("main");
    let built = karac()
        .env("KARAC_MIR_LL", d.join("main.ll"))
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let Some(()) = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    }) else {
        return;
    };
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(out(&ran), "16\n9\n");
    let ll = d.join("main.ll");
    let unchecked = function(&ll, "total_unchecked");
    assert!(!unchecked.is_empty(), "no total_unchecked in the IR");
    // The loop guard `i < n` is signed; a bounds check is `<u len`.
    let checks = unchecked
        .iter()
        .filter(|l| l.contains("icmp ult i64"))
        .count();
    assert_eq!(checks, 0, "{}", unchecked.join("\n"));
    let checked = function(&ll, "total_checked");
    assert!(
        checked.iter().any(|l| l.contains("icmp ult i64")),
        "{}",
        checked.join("\n")
    );
    let _ = std::fs::remove_dir_all(&d);
}
