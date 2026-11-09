//! A `ref T` parameter of a by-value function is marked
//! `dereferenceable(size of T)` and `align` (B-2026-11-05-4): what a
//! borrow promises for the whole call, as Rust marks a `&T`. LLVM 22's LICM
//! no longer speculates a load on the `llvm.assume` that says the same, so
//! without the attribute kata 495's loop reloaded the `Vec`'s data pointer
//! on every trip and the x86 cmov converter branched its `min`.
//! `KARAC_MIR_DEREF_PARAM=0` leaves the attribute out.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `gaps` borrows `t` and reads it in a loop; the program prints the sum.
const SRC: &str = r#"fn gaps(t: ref Vec[i64], cap: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i + 1 < t.len() {
        s += (t[i + 1] - t[i]).min(cap);
        i = i + 1;
    }
    s
}
fn main() {
    let mut t: Vec[i64] = Vec.new();
    let mut at = 0;
    for i in 0..1000 {
        at = at + (i * 7) % 13;
        t.push(at);
    }
    println(f"{gaps(t, 9)}");
}
"#;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-deref-params-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `SRC` with the attribute on or off: whether `gaps`'s definition
/// carries it, and the executable (`None` without a linker).
fn build(dir: &Path, on: bool) -> (bool, Option<PathBuf>) {
    let tag = if on { "on" } else { "off" };
    let file = dir.join(format!("{tag}.kara"));
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.join(tag);
    let ll = dir.join(format!("{tag}.ll"));
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if !on {
        c.env("KARAC_MIR_DEREF_PARAM", "0");
    }
    let built = c
        .env("KARAC_MIR_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("{tag}: no IR\n{why}"));
    let def = ir
        .lines()
        .find(|l| l.starts_with("define") && l.contains("@kara.gaps("))
        .unwrap_or_else(|| panic!("{tag}: no definition of gaps\n{ir}"));
    // A `ref Vec` is `noalias` too, its header frozen by the borrow
    // (B-2026-11-09-8), and `KARAC_MIR_DEREF_PARAM=0` drops both.
    let marked = def.contains("ptr align 8 dereferenceable(24) %0")
        || def.contains("ptr noalias align 8 dereferenceable(24) %0");
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    });
    (marked, ran.map(|()| exe))
}

#[test]
fn mir_ref_params_are_dereferenceable_and_aligned() {
    let dir = scratch();
    let (on, exe) = build(&dir, true);
    let (off, exe_off) = build(&dir, false);
    assert!(
        on,
        "the `ref Vec` parameter carries dereferenceable(24) align 8"
    );
    assert!(!off, "KARAC_MIR_DEREF_PARAM=0 leaves it out");
    let mut outs = Vec::new();
    for exe in [exe, exe_off].into_iter().flatten() {
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        outs.push(String::from_utf8_lossy(&out.stdout).to_string());
    }
    for out in &outs {
        // The Python mirror's sum of min(t[i + 1] - t[i], 9).
        assert_eq!(out, "5538\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
