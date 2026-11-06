//! A by-value function's `bool` parameters and `bool` return are marked
//! `range(i8 0, 2)` (B-2026-11-05-25): a `bool` travels as an `i8` that only
//! ever holds 0 or 1, which LLVM cannot see across a call.
//! `KARAC_MIR_RANGE_ATTR=0` leaves the attribute out.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// `same` is recursive, so its definition stays in the module; it takes a
/// `bool` and returns one. The program prints how many depths agree.
const SRC: &str = r#"fn same(a: i64, b: i64, strict: bool) -> bool {
    if a == 0 or b == 0 {
        return a == b or not strict;
    }
    same(a - 1, b - 1, strict)
}
fn main() {
    let mut n = 0;
    for i in 0..50 {
        if same(i, i % 7 + i / 7 * 7, true) {
            n += 1;
        }
        if same(i, 3, false) {
            n += 1;
        }
    }
    println(f"{n}");
}
"#;

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-bool-range-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `SRC` with the attribute on or off: `same`'s definition line, and
/// the executable (`None` without a linker).
fn build(dir: &Path, on: bool) -> (String, Option<PathBuf>) {
    let tag = if on { "on" } else { "off" };
    let file = dir.join(format!("{tag}.kara"));
    std::fs::write(&file, SRC).unwrap();
    let exe = dir.join(tag);
    let ll = dir.join(format!("{tag}.ll"));
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if !on {
        c.env("KARAC_MIR_RANGE_ATTR", "0");
    }
    let built = c
        .env("KARAC_MIR_LL", &ll)
        .env("KARAC_AUTO_PAR", "0")
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ir = std::fs::read_to_string(&ll).unwrap_or_else(|_| panic!("{tag}: no IR\n{why}"));
    let def = ir
        .lines()
        .find(|l| l.starts_with("define") && l.contains("@kara.same("))
        .unwrap_or_else(|| panic!("{tag}: no definition of same\n{ir}"))
        .to_string();
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why)
    });
    (def, ran.map(|()| exe))
}

#[test]
fn mir_bool_params_and_returns_carry_a_range() {
    let dir = scratch();
    let (on, exe) = build(&dir, true);
    let (off, exe_off) = build(&dir, false);
    assert_eq!(
        on.matches("range(i8 0, 2)").count(),
        2,
        "the bool return and the bool parameter: {on}"
    );
    assert!(
        !off.contains("range("),
        "KARAC_MIR_RANGE_ATTR=0 leaves it out: {off}"
    );
    for exe in [exe, exe_off].into_iter().flatten() {
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        // 50 depths agree under `strict` (b == a for every i), and under
        // `not strict` every call returns true: 100.
        assert_eq!(String::from_utf8_lossy(&out.stdout), "100\n");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
