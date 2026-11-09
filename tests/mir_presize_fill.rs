//! A counting fill that `mir::presize` reserves for knows its room in
//! each push (`Vec.__push_within`), so LLVM can drop the push's capacity
//! check and growth path: a fill of one value becomes a `memset`, as on
//! the legacy backend (kata 20's bracket runs, B-2026-10-31-42). The
//! second run refills the same vector after a counter reset, to a bound
//! `depth - 1`. The pins: the program prints the same under both
//! backends, each run's push in the fill function's IR (and its loop
//! copy's) compares the length with the room (`%within`), and with the
//! pass off
//! (`KARAC_MIR_PRESIZE=0`) none does, so the pin can fail.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SRC: &str = r#"fn runs(depth: i64, op: u8, cl: u8) -> Vec[u8] {
    let mut buf: Vec[u8] = Vec.new();
    let mut i = 0;
    while i < depth {
        buf.push(op);
        i = i + 1;
    }
    i = 0;
    while i < depth - 1 {
        buf.push(cl);
        i = i + 1;
    }
    buf
}
fn main() {
    let mut total = 0;
    let mut k = 0;
    while k < 3 {
        let b = runs(5 + k, 40 + k as u8, 41);
        total = total + b.len() * 1000 + b[0] as i64 + b[b.len() - 1] as i64;
        k = k + 1;
    }
    println(total);
    println(runs(0, 1, 2).len());
}
"#;

/// The expected lines are the program's arithmetic: lengths 9, 11 and 13
/// (33000), first bytes 40 + 41 + 42, last bytes 41 each; `runs(0, ..)`
/// pushes nothing.
const EXPECTED: &str = "33246\n0\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-presize-fill-{name}-{}", std::process::id()));
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

/// `runs`' IR and its program's output, built with `presize` set.
fn build(d: &Path, presize: &str) -> Option<(Vec<String>, String)> {
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .env("KARAC_MIR_PRESIZE", presize)
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
    Some((function(&ll, "runs"), out(&ran)))
}

#[test]
fn a_presized_fill_skips_the_capacity_check() {
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
    let Some((ir, ran)) = build(&d, "1") else {
        return;
    };
    assert_eq!(ran, EXPECTED);
    assert!(!ir.is_empty(), "no runs in the IR");
    let within = |ir: &[String]| {
        ir.iter()
            .filter(|l| l.trim_start().starts_with("%within") && l.contains("icmp slt"))
            .count()
    };
    // Each run's push, and the push of the copy `presize` makes of its
    // loop for when the room covers every trip (B-2026-11-09-26).
    assert_eq!(within(&ir), 4, "{}", ir.join("\n"));
    let Some((off, ran)) = build(&d, "0") else {
        return;
    };
    assert_eq!(ran, EXPECTED);
    assert!(!off.is_empty(), "no runs in the IR");
    assert_eq!(within(&off), 0, "{}", off.join("\n"));
    let _ = std::fs::remove_dir_all(&d);
}
