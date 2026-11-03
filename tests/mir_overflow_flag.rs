//! A checked `x + 1` / `x - 1` is lowered as a compare against the bound
//! and a `nsw` step, for LLVM's loop analyses; after the passes,
//! `mir_aot::overflow_flag` turns the compare back into the overflow
//! intrinsic's flag, so a target with flags runs `adds; b.vs` rather than
//! `cmp; b.eq; add` (B-2026-11-02-3: kata 242's byte loop). The pins: the
//! pass fires on a counting loop and on the steps that overflow, and the
//! programs print and fail the same with it on and off.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

const COUNT: &str = r#"fn tally(s: ref String, t: ref String) -> i64 {
    let sb = s.bytes();
    let tb = t.bytes();
    let mut count: Array[i64, 26] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
    ];
    let mut i = 0;
    while i < sb.len() {
        count[(sb[i] as i64) - 97] = count[(sb[i] as i64) - 97] + 1;
        count[(tb[i] as i64) - 97] = count[(tb[i] as i64) - 97] - 1;
        i = i + 1;
    }
    let mut odd = 0;
    for c in count {
        if c != 0 {
            odd = odd + 1;
        }
    }
    odd
}

fn main() {
    println(tally("listen".to_string(), "silent".to_string()));
    println(tally("rat".to_string(), "car".to_string()));
}
"#;

const UP: &str = r#"fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(9223372036854775805);
    let mut n = 0;
    while n < 5 {
        a[0] = a[0] + 1;
        println(a[0]);
        n = n + 1;
    }
}
"#;

const DOWN: &str = r#"fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(-9223372036854775806);
    let mut n = 0;
    while n < 5 {
        a[0] = a[0] - 1;
        println(a[0]);
        n = n + 1;
    }
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-overflow-flag-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// What the build of `src` with the pass set to `flag` printed, its exit
/// code, its stderr, and how many steps the pass reported rewriting.
fn build(d: &Path, src: &str, flag: &str) -> Option<(String, Option<i32>, String, usize)> {
    let file = d.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = d.join(format!("main-{flag}"));
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_MIR_OVERFLOW_FLAG", flag)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    let rewrote = err
        .lines()
        .filter_map(|l| l.strip_prefix("overflow-flag: "))
        .filter_map(|n| n.trim().parse::<usize>().ok())
        .sum();
    let ran = Command::new(&exe).output().unwrap();
    Some((
        String::from_utf8_lossy(&ran.stdout).to_string(),
        ran.status.code(),
        String::from_utf8_lossy(&ran.stderr).to_string(),
        rewrote,
    ))
}

#[test]
fn a_counting_loops_checked_steps_get_their_flag() {
    let d = dir("count");
    let Some((out, code, _, rewrote)) = build(&d, COUNT, "report") else {
        return;
    };
    assert_eq!(out, "0\n2\n");
    assert_eq!(code, Some(0));
    assert!(rewrote >= 2, "rewrote {rewrote}");
    let Some((off, _, _, none)) = build(&d, COUNT, "0") else {
        return;
    };
    assert_eq!(off, out);
    assert_eq!(none, 0);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_flagged_step_still_stops_at_the_bound() {
    for (name, src, want) in [
        ("up", UP, "9223372036854775806\n9223372036854775807\n"),
        ("down", DOWN, "-9223372036854775807\n-9223372036854775808\n"),
    ] {
        let d = dir(name);
        let Some((out, code, err, rewrote)) = build(&d, src, "report") else {
            return;
        };
        assert_eq!(out, want, "{name}");
        assert_eq!(code, Some(101), "{name}: {err}");
        assert!(err.contains("integer overflow"), "{name}: {err}");
        assert!(rewrote >= 1, "{name}: rewrote {rewrote}");
        let Some((off, off_code, _, _)) = build(&d, src, "0") else {
            return;
        };
        assert_eq!((off, off_code), (out, code), "{name}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
