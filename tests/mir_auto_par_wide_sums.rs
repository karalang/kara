//! An auto-par loop's signed sum whose runs leave the sum's type on their
//! own, on either side, at the first term of a chain or the second, gives
//! the sequential sum, and overflows only where the sequential loop does,
//! on the MIR interpreter and on the built binary. (Pins for keeping the
//! tasks' sums narrower than `i128`, B-2026-10-26-14.)
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn source(dir: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("karac-auto-par-wide-{dir}-{}", std::process::id()));
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
        c.env_remove("KARAC_AUTO_PAR");
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

/// A sum of `i32`s whose runs leave `i32` on both sides (`+2e9 +2e9`,
/// `-2e9 -2e9`) where the in-order sum, from `-2e9`, never does; from
/// `-1e9` the in-order sum overflows at its second term.
#[test]
fn mir_auto_par_sum_whose_runs_leave_its_type_on_either_side() {
    let src = r#"fn term(v: ref Vec[i32], i: i64) -> i32 {
    let mut s: i32 = 0;
    let mut j = 0;
    while j < 300 {
        s = s + ((j % 2) as i32) - ((j % 2) as i32);
        j = j + 1;
    }
    v[i] + s
}
fn main() {
    let mut v: Vec[i32] = Vec.new();
    for i in 0..64 {
        if i % 4 < 2 {
            v.push(2000000000);
        } else {
            v.push(-2000000000);
        }
    }
    let mut total: i32 = START;
    for i in 0..64 {
        total = total + term(v, i);
    }
    println(f"{total}");
}
"#;
    let ok = src.replace("START", "-2000000000");
    let (out, code, err) =
        same_both_ways("i32", &ok, &["main@20"], &[], &["main@20 (total at 21)"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "-2000000000\n");
    let over = src.replace("START", "-1000000000");
    let (out, code, err) = same_both_ways(
        "i32-over",
        &over,
        &["main@20"],
        &[],
        &["main@20 (total at 21)"],
    );
    assert_eq!(out, "");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow") && err.contains(":21:"), "{err}");
}

/// A chain `acc = acc + a + b` whose runs leave `i64` at `b`, after `a`
/// kept them in it, and come back (`6e18 + 6e18`, then `-6e18 - 6e18`).
#[test]
fn mir_auto_par_chain_whose_runs_leave_its_type_at_its_second_term() {
    let src = r#"fn half(i: i64, k: i64) -> i64 {
    let mut s = 0;
    let mut j = 0;
    while j < 300 {
        s = s + (j % 3) - (j % 3);
        j = j + 1;
    }
    if i % 2 == 0 { k + s } else { 0 - k + s }
}
fn main() {
    let mut acc: i64 = -6000000000000000000;
    for i in 0..64 {
        acc = acc + half(i, 6000000000000000000) + half(i, 6000000000000000000);
    }
    println(f"{acc}");
}
"#;
    let (out, code, err) =
        same_both_ways("chain", src, &["main@12"], &[], &["main@12 (acc at 13)"]);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, "-6000000000000000000\n");
}
