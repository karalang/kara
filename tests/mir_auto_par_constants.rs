//! An auto-par task binds a captured `let` of a literal (no `mut`) to the
//! constant instead of reading it through its environment, so its code
//! folds the constant as the sequential loop's does (B-2026-10-26-14: kata
//! 4's bounds, read through the environment, left its binary search
//! unfolded, 0.38x its sequential build on one worker). Literals of every
//! scalar kind, a shadowed name and a `mut` one (which stays a capture)
//! give the sequential program's output, on the MIR interpreter and on the
//! built binary.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn source(dir: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-auto-par-const-{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

/// `size: false` turns the size half of the cost gate down, so the inner
/// loops are candidates as well.
fn karac(auto: bool, size: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    if auto {
        c.env("KARAC_AUTO_PAR", "1")
            .env("KARAC_AUTO_PAR_DEBUG", "1");
        if !size {
            c.env("KARAC_AUTO_PAR_SIZE", "0");
        }
    } else {
        c.env("KARAC_AUTO_PAR", "0");
    }
    c
}

fn interp_run(name: &str, src: &str, auto: bool, size: bool) -> Output {
    let (dir, file) = source(name, src);
    let out = karac(auto, size)
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac build --mir` and the binary run on four workers, or `None` when
/// the runtime archive does not link; the build's stderr comes back with it.
fn build_and_run(name: &str, src: &str, auto: bool, size: bool) -> Option<(Output, String)> {
    let (dir, file) = source(name, src);
    let exe = dir.join("main");
    let built = karac(auto, size)
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "{name} does not build: {err}"
    );
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err.clone())
    })?;
    let out = Command::new(&exe)
        .env("KARAC_THREADS", "4")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, err))
}

/// stdout, exit code, and stderr without the auto-par report.
fn shown(o: &Output) -> (String, Option<i32>, String) {
    let err: Vec<&str> = std::str::from_utf8(&o.stderr)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("auto-par:"))
        .collect();
    (
        String::from_utf8(o.stdout.clone()).unwrap(),
        o.status.code(),
        err.join("\n"),
    )
}

/// The auto-par report's line for `what` (`loops as tasks`, `reductions`).
fn reported(stderr: &[u8], what: &str) -> String {
    let prefix = format!("auto-par: {what}: ");
    std::str::from_utf8(stderr)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// The loops an auto-par report names (`instance@line`), without their
/// reductions.
fn names(line: &str) -> Vec<&str> {
    line.split(", ").filter(|l| !l.is_empty()).collect()
}

/// Runs `src` both ways on both backends, under either cost gate: the same
/// output each time, and the auto-par builds run each of `outer` as tasks
/// with its reduction in `reds`, and none of `inner`. (Loops that build
/// the input may run as tasks too, depending on the gate.)
fn same_both_ways(
    name: &str,
    src: &str,
    outer: &[&str],
    inner: &[&str],
    reds: &[&str],
) -> (String, Option<i32>, String) {
    let check = |stderr: &[u8], what: &str| {
        let loops = reported(stderr, "loops as tasks");
        let tasked = names(&loops);
        for o in outer {
            assert!(tasked.contains(o), "{what}: {o} in {loops}");
        }
        for i in inner {
            assert!(!tasked.contains(i), "{what}: {i} not in {loops}");
        }
        let got = reported(stderr, "reductions");
        for r in reds {
            assert!(names(&got).contains(r), "{what}: {r} in {got}");
        }
    };
    let seq = interp_run(name, src, false, true);
    for size in [true, false] {
        let what = format!("{name}, size {size}");
        let par = interp_run(name, src, true, size);
        assert_eq!(shown(&seq), shown(&par), "{what}: interpreter");
        check(&par.stderr, &what);
        if let (Some((seq_b, _)), Some((par_b, built))) = (
            build_and_run(name, src, false, true),
            build_and_run(name, src, true, size),
        ) {
            assert_eq!(shown(&seq_b), shown(&par_b), "{what}: binary");
            assert_eq!(shown(&seq), shown(&seq_b), "{what}: interpreter and binary");
            check(built.as_bytes(), &what);
        }
    }
    shown(&seq)
}

/// Constants of several types read in a loop's tasks, one shadowed and
/// one `mut`, beside a reduction.
#[test]
fn mir_auto_par_task_reads_captured_constants() {
    let src = r#"fn work(n: i64, k: i64) -> i64 {
    let mut s = 0;
    let mut j = 0;
    while j < n {
        s = s + (j * k) % 7;
        j = j + 1;
    }
    s
}
fn main() {
    let n = 300;
    let k = -3;
    let small: i32 = 70000;
    let byte = 200u8;
    let flag = true;
    let ch = 'q';
    let half = 0.5;
    let shadow = 4;
    let shadow = shadow * 10;
    let mut later = 1;
    later = later + 6;
    let mut total = 0;
    for i in 0..64 {
        let f = if flag { 1 } else { 0 };
        let h = if half > 0.25 { 2 } else { 3 };
        let c = if ch == 'q' { 5 } else { 6 };
        total = total + work(n + i, k) + (small as i64) + (byte as i64) + f + h + c + shadow + later;
    }
    println(f"{total}");
}
"#;
    let (out, code, err) =
        same_both_ways("consts", src, &["main@23"], &[], &["main@23 (total at 27)"]);
    assert_eq!(code, Some(0), "{err}");
    let work = |n: i64, k: i64| (0..n).map(|j| (j * k) % 7).sum::<i64>();
    let total: i64 = (0..64)
        .map(|i| work(300 + i, -3) + 70000 + 200 + 1 + 2 + 5 + 40 + 7)
        .sum();
    assert_eq!(out, format!("{total}\n"));
}
