//! `par for` iterations run as tasks of one group (M4 item 12): each
//! iteration's body is a task, the handles are joined in iteration order,
//! and the loop's value is the `Vec` of the bodies' values in that order
//! (core semantics §11.1). Checked on the MIR interpreter and on the built
//! binary.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn source(dir: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-par-for-{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (dir, file)
}

/// `karac __mir-run`: its output and how long it ran.
fn interp_run(name: &str, src: &str) -> (Output, Duration) {
    let (dir, file) = source(&format!("i-{name}"), src);
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    let took = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    (out, took)
}

/// `karac build --mir` and the binary run, or `None` when the runtime
/// archive does not link.
fn build_and_run(name: &str, src: &str) -> Option<(Output, Duration)> {
    let (dir, file) = source(&format!("b-{name}"), src);
    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
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
        Err(err)
    })?;
    let start = Instant::now();
    let out = Command::new(&exe).output().unwrap();
    let took = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    Some((out, took))
}

/// Each run that happened: the interpreter's, and the binary's when it
/// links, with how long each took.
fn runs(name: &str, src: &str) -> Vec<(String, Duration)> {
    let mut out = Vec::new();
    let (i, took) = interp_run(name, src);
    assert!(
        i.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&i.stderr)
    );
    out.push((String::from_utf8(i.stdout).unwrap(), took));
    if let Some((b, took)) = build_and_run(name, src) {
        assert!(
            b.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&b.stderr)
        );
        out.push((String::from_utf8(b.stdout).unwrap(), took));
    }
    out
}

/// The values come back in iteration order, whatever each body holds: a
/// struct built in the task, a nested `par for`, an element borrowed from
/// the iterated `Vec`, and a generic function's loop.
#[test]
fn mir_par_for_values_keep_iteration_order() {
    let src = r#"struct P { name: String, n: i64 }
fn total[T: Copy](xs: ref Vec[T], f: Fn(T) -> i64) -> Vec[i64] {
    let r: Vec[i64] = par for x in xs.iter() { f(*x) };
    r
}
fn label(s: ref String, k: i64) -> String { f"{s}-{k}" }
fn main() {
    let v = vec![3, 1, 4, 1, 5];
    let t = total(v, |x| x * 3);
    println(f"{t.len()} {t[2]}");
    let ps: Vec[P] = par for i in 0..3 {
        let mut s = String.new();
        for _ in 0..i { s.push_str("ab"); }
        P { name: s, n: i }
    };
    for p in ps { println(f"{p.n}:{p.name}"); }
    let grid: Vec[Vec[i64]] = par for r in 0..3 {
        let row: Vec[i64] = par for c in 0..3 { r * 10 + c };
        row
    };
    println(f"{grid[2][1]} {grid[0].len()}");
    let names = vec!["a".to_string(), "bb".to_string()];
    let base = "n".to_string();
    let ls: Vec[String] = par for s in names { label(base, s.len() as i64) };
    println(f"{ls[0]} {ls[1]}");
}
"#;
    for (out, _) in runs("order", src) {
        assert_eq!(out, "5 12\n0:\n1:ab\n2:abab\n21 3\nn-1 n-2\n");
    }
}

/// The iterations wait at once: four sleeps of 800, 600, 400 and 200 ms
/// end in about the longest, not their 2000 ms sum, and the values still
/// come back in iteration order. With `limit: 1` they run one at a time.
/// The interpreter's time includes the debug `karac`'s own check and
/// lowering, which now takes most of a second, so each run is measured
/// against the same program with no sleep.
#[test]
fn mir_par_for_iterations_wait_concurrently() {
    let src = r#"fn main() {
    let v: Vec[i64] = par for i in 0..4 { sleep_ms((4 - i) * 200); i };
    println(f"{v[0]} {v[1]} {v[2]} {v[3]}");
}
"#;
    let base = runs("base", &src.replace("(4 - i) * 200", "0 * i"));
    for ((out, took), (_, base)) in runs("concurrent", src).into_iter().zip(base) {
        assert_eq!(out, "0 1 2 3\n");
        assert!(
            took < base + Duration::from_millis(1300),
            "{took:?} against {base:?} with no sleep"
        );
    }
    let one = src.replace("par for", "par(limit: 1) for");
    for (out, took) in runs("limit", &one) {
        assert_eq!(out, "0 1 2 3\n");
        assert!(took >= Duration::from_millis(2000), "{took:?}");
    }
}
