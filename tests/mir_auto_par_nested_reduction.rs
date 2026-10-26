//! An auto-par loop whose reduction is written inside an inner loop
//! (B-2026-10-26-14): the outer loop runs as tasks, each folding its run of
//! iterations, inner loops and all, and the join folds the runs in order.
//! Before, the builder looked for the reduction's statement only at the
//! outer body's top, under `if` and `match`, so it kept the outer loop in
//! order and split the inner one instead, once per outer iteration: kata
//! 332 ran 0.57x its sequential build on four workers. The output, exit
//! code and panic message are the sequential program's, on the MIR
//! interpreter and on the built binary.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

fn source(dir: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-auto-par-nested-{dir}-{}",
        std::process::id()
    ));
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

/// A signed sum written in a counting `while` nested in the outer one, as
/// in kata 332, and one written in an inner `for`: the outer loops run as
/// tasks, the inner ones in order inside them.
#[test]
fn mir_auto_par_reduction_in_an_inner_loop_folds_in_the_outer_one() {
    let src = r#"fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut r = 0;
    while r < 40 {
        let mut row: Vec[i64] = Vec.new();
        for c in 0..(r % 7 + 1) {
            row.push(r * 10 + c);
        }
        rows.push(row);
        r = r + 1;
    }
    let mut sink = 0;
    let mut it = 0;
    while it < rows.len() {
        let row = rows[it].clone();
        let mut i = 0;
        while i < row.len() {
            sink = sink + (i + 1) * row[i];
            i = i + 1;
        }
        it = it + 1;
    }
    println(f"{sink}");
    let mut x = 0;
    for k in 0..rows.len() {
        for v in rows[k] {
            x ^= v * 31 + k;
        }
    }
    println(f"{x}");
}
"#;
    let (out, code, err) = same_both_ways(
        "inner",
        src,
        &["main@14", "main@25"],
        &["main@17", "main@26"],
        &["main@14 (sink at 18)", "main@25 (x at 27)"],
    );
    assert_eq!(code, Some(0), "{err}");
    let mut sink = 0i64;
    let mut x = 0i64;
    for r in 0..40i64 {
        for c in 0..(r % 7 + 1) {
            sink += (c + 1) * (r * 10 + c);
            x ^= (r * 10 + c) * 31 + r;
        }
    }
    assert_eq!(out, format!("{sink}\n{x}\n"));
}

/// A signed sum in an inner loop whose runs overflow on their own
/// (`i64.max`, then 1s, then -1s, a row each) gives the sequential sum, and
/// with a larger start overflows where the in-order sum does, at the inner
/// loop's statement.
#[test]
fn mir_auto_par_inner_loop_sum_overflows_only_where_the_loop_does() {
    const OUTER: &[&str] = &["main@14"];
    const INNER: &[&str] = &["main@16"];
    const REDS: &[&str] = &["main@14 (acc at 17)"];
    let src = r#"fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut top: Vec[i64] = Vec.new();
    top.push(9223372036854775807);
    rows.push(top);
    for r in 0..8 {
        let mut row: Vec[i64] = Vec.new();
        for _ in 0..1000 {
            row.push(if r == 0 { 1 } else { -1 });
        }
        rows.push(row);
    }
    let mut acc = -1010;
    for k in 0..rows.len() {
        let row = rows[k].clone();
        for i in 0..row.len() {
            acc = acc + row[i];
        }
    }
    println(f"{acc}");
}
"#;
    let (out, code, err) = same_both_ways("wide", src, OUTER, INNER, REDS);
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(out, format!("{}\n", i64::MAX - 1010 + 1000 - 7000));
    let over = src.replace("let mut acc = -1010;", "let mut acc = 10;");
    let (out, code, err) = same_both_ways("wide-over", &over, OUTER, INNER, REDS);
    assert_eq!(out, "");
    assert_eq!(code, Some(101), "{err}");
    assert!(err.contains("overflow"), "{err}");
}
