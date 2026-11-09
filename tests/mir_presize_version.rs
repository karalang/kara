//! A presized fill whose bound is an `i64` gets a second copy of its loop,
//! taken when the vector's length plus the trips fits the reservation:
//! there each push's reach is the exact final length, so LLVM can count
//! the pushes and drop the grow test, and the loop vectorizes
//! (B-2026-11-09-26). Kata 484's `(1..=n).collect()` with `n` a collected
//! vector's length kept its grow test until the copy's step was emitted as
//! a no-overflow add. The pins: values on both copies' edges (seed pushes,
//! an empty and a negative bound, a nonzero start) on the interpreter and
//! the built binary, and the built fill's loop vectorized, which it is not with
//! `KARAC_MIR_PRESIZE_VERSION=0`.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const FILLS: &str = r#"fn fill(s: String) -> Vec[i64] {
    let letters: Vec[char] = s.chars().collect();
    let n = letters.len() + 1;
    let perm: Vec[i64] = (1..=n).collect();
    perm
}
fn seeded(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(-1);
    for i in 0..n {
        v.push(i * 2);
    }
    v
}
fn main() {
    let p = fill("ab".repeat(500));
    println(f"{p.len()} {p[0]} {p[500]} {p[1000]}");
    println(f"{seeded(4)} {seeded(0)} {seeded(-3)}");
    let mut w: Vec[i64] = Vec.new();
    let k = p.len();
    for i in 5..k {
        w.push(i);
    }
    println(f"{w.len()} {w[0]} {w[w.len() - 1]}");
}
"#;

const WANT: &str = "1001 1 501 1001\n[-1, 0, 2, 4, 6] [-1] [-1]\n996 5 1000\n";

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

/// Builds `file` with versioning on or off and returns the optimized IR,
/// or `None` when the runtime archive is missing.
fn build(d: &std::path::Path, file: &str, version: &str) -> Option<String> {
    let exe = d.join(format!("main{version}"));
    let ll = d.join(format!("opt{version}.ll"));
    let built = karac()
        .env("KARAC_MIR_PRESIZE_VERSION", version)
        .env("KARAC_MIR_CARRY_STORE", "report")
        .env("KARAC_MIR_OPT_LL", &ll)
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    })?;
    let ran = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        WANT,
        "version={version}"
    );
    let mut ir = String::new();
    for k in 0.. {
        match std::fs::read_to_string(format!("{}.{k}", ll.display())) {
            Ok(m) => ir.push_str(&m),
            Err(_) => break,
        }
    }
    assert!(!ir.is_empty(), "no optimized module was kept");
    Some(ir)
}

#[test]
fn mir_presize_version_vectorizes_a_counted_fill() {
    let d = std::env::temp_dir().join(format!("karac-presize-version-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, FILLS).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    let Some(on) = build(&d, file, "1") else {
        return;
    };
    let Some(off) = build(&d, file, "0") else {
        return;
    };
    // The loop vectorizer's induction vector names a vectorized loop
    // (`store <4 x i64> <i64 -1, ...>` is only `seeded(4)` unrolled).
    let vectorized = |ir: &str| ir.contains("%vec.ind");
    assert!(vectorized(&on), "no versioned fill was vectorized");
    assert!(
        !vectorized(&off),
        "the control arm vectorizes a fill without the copy"
    );
    let _ = std::fs::remove_dir_all(&d);
}
