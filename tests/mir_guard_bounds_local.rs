//! An index into a local vector, under a loop guard against that vector's
//! length, checks only its sign on mir-llvm (B-2026-11-08-61). Kata 459's
//! `for i in d..n { cs[i] .. }` with `n = cs.len()` and kata 484's
//! `while i < letters.len() and letters[i] == 'D'` index a vector made
//! once by `collect` and never written after, so its length is the one
//! the guard compared with. The pins: both shapes check `>= 0` alone and
//! answer as before, the proof is gone under `KARAC_MIR_GUARD_BOUNDS=0`,
//! and a vector reassigned or shortened inside the loop keeps its full
//! check and still panics.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `make` is a call in the IR, so the vectors below are made by a call,
/// as `collect` makes them in the katas.
const MAKE: &str = r#"fn make(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..k {
        v.push(i % 3 / 2);
    }
    v
}
"#;

/// Kata 459's shape: `cs[i]` is bounded by the `for` range's end, a copy
/// of `n = cs.len()`; `cs[i - d]` is not, and keeps its full check.
const REPEATS: &str = r#"fn repeats(k: i64) -> i64 {
    let cs: Vec[i64] = make(k);
    let n = cs.len();
    let mut found = 0;
    for d in 1..n / 2 + 1 {
        let mut same = true;
        for i in d..n {
            if cs[i] != cs[i - d] {
                same = false;
                break;
            }
        }
        if same {
            found += 1;
        }
    }
    found
}
fn main() {
    println(f"{repeats(12)} {repeats(13)}");
}
"#;

/// Kata 484's shape: each guard re-reads `letters.len()`.
const RUNS: &str = r#"fn runs(k: i64) -> i64 {
    let letters: Vec[i64] = make(k);
    let mut i = 0;
    let mut best = 0;
    while i < letters.len() {
        if letters[i] == 0 {
            let start = i;
            while i < letters.len() and letters[i] == 0 {
                i += 1;
            }
            if i - start > best {
                best = i - start;
            }
        } else {
            i += 1;
        }
    }
    best
}
fn main() {
    println(f"{runs(12)} {runs(2)}");
}
"#;

/// The vector is replaced by a shorter one inside the loop.
const REASSIGNED: &str = r#"fn swapped(k: i64) -> i64 {
    let mut v: Vec[i64] = make(k);
    let n = v.len();
    let mut s = 0;
    for i in 0..n {
        if i == 1 {
            v = make(1);
        }
        s += v[i];
    }
    s
}
fn main() {
    println("start");
    println(f"{swapped(4)}");
}
"#;

/// The vector loses an element each trip.
const POPPED: &str = r#"fn popped(k: i64) -> i64 {
    let mut v: Vec[i64] = make(k);
    let n = v.len();
    let mut s = 0;
    for i in 0..n {
        s += v[i];
        let _ = v.pop();
    }
    s
}
fn main() {
    println("start");
    println(f"{popped(4)}");
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "karac-guard-bounds-local-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

/// The interpreter's run of `src`, and the built binary's when the
/// runtime archive links, with the IR of the build at `main.ll`.
fn both(d: &Path, src: &str, env: &[(&str, &str)]) -> (Output, Option<Output>) {
    let file = d.join("main.kara");
    std::fs::write(&file, format!("{MAKE}{src}")).unwrap();
    let file = file.to_str().unwrap();
    let interp = karac().args(["__mir-run", file]).output().unwrap();
    let exe = d.join("main");
    let mut build = karac();
    build.env("KARAC_MIR_LL", d.join("main.ll"));
    for (k, v) in env {
        build.env(k, v);
    }
    let built = build
        .args(["build", file, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let why = String::from_utf8_lossy(&built.stderr).to_string();
    let ran = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(why.clone())
    })
    .map(|()| Command::new(&exe).output().unwrap());
    (interp, ran)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// The `>= 0` and `<u len` compares in function `name`'s IR.
fn checks(ll: &Path, name: &str) -> (usize, usize, String) {
    let ir = std::fs::read_to_string(ll).unwrap();
    let head = format!("@kara.{name}(");
    let f: Vec<&str> = ir
        .lines()
        .skip_while(|l| !(l.starts_with("define") && l.contains(&head)))
        .take_while(|l| *l != "}")
        // A loop counter's monotone fact (`mir::monotone`) is an assume.
        .filter(|l| !l.trim_start().starts_with("%mono.fact"))
        .collect();
    assert!(!f.is_empty(), "no {name} in the IR");
    let sign = f.iter().filter(|l| l.contains("icmp sge i64")).count();
    let full = f.iter().filter(|l| l.contains("icmp ult i64")).count();
    (sign, full, f.join("\n"))
}

#[test]
fn mir_guard_bounds_local_vec_indexes_check_only_their_sign() {
    for (src, f, want, proved, off) in [
        (REPEATS, "repeats", "2 2\n", (1, 1), (0, 2)),
        (RUNS, "runs", "2 2\n", (2, 0), (0, 2)),
    ] {
        for (env, counts) in [
            (&[][..], proved),
            (&[("KARAC_MIR_GUARD_BOUNDS", "0")][..], off),
        ] {
            let d = dir(&format!("{f}-{}", env.len()));
            let (interp, built) = both(&d, src, env);
            assert_eq!(out(&interp), want, "{f}");
            let Some(built) = built else { return };
            assert_eq!(out(&built), want, "{f}");
            let (sign, full, ir) = checks(&d.join("main.ll"), f);
            assert_eq!((sign, full), counts, "{f} {env:?}:\n{ir}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}

/// A vector its loop replaces or shortens keeps the full check, and the
/// index past its new end still panics.
#[test]
fn mir_guard_bounds_local_vec_written_in_the_loop_keeps_its_check() {
    for (name, src, f) in [
        ("reassigned", REASSIGNED, "swapped"),
        ("popped", POPPED, "popped"),
    ] {
        let d = dir(name);
        let (interp, built) = both(&d, src, &[]);
        if built.is_some() {
            let (sign, full, ir) = checks(&d.join("main.ll"), f);
            assert_eq!((sign, full), (0, 1), "{name}:\n{ir}");
        }
        let runs = std::iter::once(("interp", interp)).chain(built.map(|b| ("built", b)));
        for (how, o) in runs {
            let err = String::from_utf8_lossy(&o.stderr);
            assert_eq!(out(&o), "start\n", "{name} {how}: {err}");
            assert_eq!(o.status.code(), Some(101), "{name} {how}: {err}");
            assert!(err.contains("index out of bounds"), "{name} {how}: {err}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
