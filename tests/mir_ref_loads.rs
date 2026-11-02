//! A reference is loaded as non-null, aligned and dereferenceable for its
//! pointee's size (B-2026-11-02-17): what a `&T` / `&mut T` promises, told
//! to LLVM so it may hoist reads through the reference out of a loop. An
//! auto-par task's captured `Vec` was the case that measured: without the
//! facts, LICM could not hoist the buffer's load and reloaded it on every
//! iteration of kata 9's task. `KARAC_MIR_REF_LOADS=0` turns them off.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `total` borrows `v` and reads it in a loop; the program prints the sum.
const SRC: &str = r#"fn total(v: Vec[i64]) -> i64 {
    let mut s: i64 = 0;
    let mut i = 0;
    while i < v.len() {
        s = s + v[i];
        i = i + 1;
    }
    s
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..1000 {
        v.push(i * 3);
    }
    println(f"{total(v)}");
}
"#;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-ref-loads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `SRC` with the facts on or off: the IR's reference loads that
/// carry all three facts, and the executable (`None` without a linker).
fn build(dir: &Path, on: bool) -> (usize, Option<PathBuf>) {
    let tag = if on { "on" } else { "off" };
    let file = dir.join(format!("{tag}.kara"));
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.join(tag);
    let ll = dir.join(format!("{tag}.ll"));
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if !on {
        c.env("KARAC_MIR_REF_LOADS", "0");
    }
    let built = c
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("{tag}: no IR\n{why}"));
    let facts = ir
        .lines()
        .filter(|l| {
            l.contains("= load ptr")
                && l.contains("!nonnull")
                && l.contains("!dereferenceable")
                && l.contains("!align")
        })
        .count();
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    });
    (facts, ran.map(|()| exe))
}

#[test]
fn mir_ref_loads_carry_nonnull_dereferenceable_align() {
    let dir = scratch();
    let (on, exe) = build(&dir, true);
    let (off, exe_off) = build(&dir, false);
    assert!(on > 0, "a reference load carries the facts");
    assert_eq!(off, 0, "KARAC_MIR_REF_LOADS=0 drops them");
    for exe in [exe, exe_off].into_iter().flatten() {
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "1498500\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
