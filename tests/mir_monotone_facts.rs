//! A loop counter every write of which is a checked step one way gets
//! `assume(x >= init)` (or `<=`) at its loop's header, `init` its value on
//! entry, so LLVM can drop the checks on it that only that bound decides
//! (`mir::monotone`, B-2026-11-01-19: kata 88's merge, whose cursors are
//! stepped conditionally and so are no recurrence LLVM can range). The
//! pins: the program prints the same under both backends, the merge's IR
//! states a fact (`%mono.fact`) for each of its three cursors, and with the
//! facts off (`KARAC_MIR_MONOTONE=0`) none, so the pin can fail.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SRC: &str = r#"fn merge(nums1: mut Slice[i64], m: i64, nums2: Slice[i64], n: i64) {
    let mut i = m - 1;
    let mut j = n - 1;
    let mut k = m + n - 1;
    while j >= 0 {
        if i >= 0 and nums1[i] > nums2[j] {
            nums1[k] = nums1[i];
            i = i - 1;
        } else {
            nums1[k] = nums2[j];
            j = j - 1;
        }
        k = k - 1;
    }
}

fn report(nums1: mut Slice[i64], m: i64, nums2: Slice[i64], n: i64) {
    merge(mut nums1, m, nums2, n);
    let mut total = 0;
    for i in 0..m + n {
        total = total * 10 + nums1[i];
    }
    println(total);
}

fn main() {
    let mut a1: Array[i64, 6] = [1, 2, 3, 0, 0, 0];
    let b1: Array[i64, 3] = [2, 5, 6];
    report(mut a1, 3, b1, 3);
    let mut a2: Array[i64, 6] = [4, 5, 6, 0, 0, 0];
    let b2: Array[i64, 3] = [1, 2, 3];
    report(mut a2, 3, b2, 3);
    let mut a3: Array[i64, 1] = [0];
    let b3: Array[i64, 1] = [7];
    report(mut a3, 0, b3, 1);
}
"#;

/// Each merged array read as the digits of one number.
const EXPECTED: &str = "122356\n123456\n7\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-monotone-{name}-{}", std::process::id()));
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

/// `merge`'s IR and its program's output, built with `monotone` set.
fn build(d: &Path, monotone: &str) -> Option<(Vec<String>, String)> {
    let file = d.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let exe = d.join("main");
    let ll = d.join("main.ll");
    let built = karac()
        .env("KARAC_MIR_LL", &ll)
        .env("KARAC_MIR_MONOTONE", monotone)
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
    Some((function(&ll, "merge"), out(&ran)))
}

#[test]
fn a_merges_cursors_are_stated_monotone() {
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
    assert!(!ir.is_empty(), "no merge in the IR");
    let facts = |ir: &[String]| {
        ir.iter()
            .filter(|l| l.trim_start().starts_with("%mono.fact"))
            .count()
    };
    assert_eq!(facts(&ir), 3, "{}", ir.join("\n"));
    let Some((off, ran)) = build(&d, "0") else {
        return;
    };
    assert_eq!(ran, EXPECTED);
    assert!(!off.is_empty(), "no merge in the IR");
    assert_eq!(facts(&off), 0, "{}", off.join("\n"));
    let _ = std::fs::remove_dir_all(&d);
}
