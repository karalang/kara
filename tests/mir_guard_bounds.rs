//! An index whose upper half a loop guard proved (`hi < n` with
//! `n = v.len()`) is checked for `hi >= 0` alone on mir-llvm
//! (B-2026-10-28-33): kata 5's expand-around-center loop went from
//! 15.03G to 12.53G instructions on Linux, legacy's 12.52G. The pins: the
//! proved index runs as before and its IR compares `>= 0`, not `<u len`;
//! a negative index under the same guard still panics; and an index the
//! guard does not cover (another vector, or a counter written after the
//! compare) keeps its full check.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const EXPAND: &str = r#"fn expand(chars: ref Vec[char], lo0: i64, hi0: i64) -> Array[i64, 2] {
    let mut lo = lo0;
    let mut hi = hi0;
    let n = chars.len();
    while lo >= 0 and hi < n and chars[lo] == chars[hi] {
        lo = lo - 1;
        hi = hi + 1;
    }
    [lo + 1, hi - lo - 1]
}
fn main() {
    let mut chars: Vec[char] = Vec.new();
    for c in "abacabad".chars() {
        chars.push(c);
    }
    let mut best = 0;
    let mut at = 0;
    for i in 0..chars.len() {
        let r = expand(chars, i, i);
        if r[1] > best {
            best = r[1];
            at = r[0];
        }
    }
    println(f"{at} {best}");
}
"#;

/// `sum(v, k)` from `k`; a negative `k` reaches `v[-1]` under `i < n`.
const NEGATIVE: &str = r#"fn sum(v: ref Vec[i64], k: i64) -> i64 {
    let n = v.len();
    let mut i = k;
    let mut s = 0;
    while i < n {
        s = s + v[i];
        i = i + 1;
    }
    s
}
fn main() {
    let v: Vec[i64] = [1, 2, 3].to_vec();
    println(f"{sum(v, 0)}");
    println(f"{sum(v, -1)}");
}
"#;

/// The guard bounds `a`'s length; the index is into the shorter `b`.
const OTHER_VEC: &str = r#"fn mix(a: ref Vec[i64], b: ref Vec[i64]) -> i64 {
    let n = a.len();
    let mut i = 0;
    let mut s = 0;
    while i < n {
        s = s + b[i];
        i = i + 1;
    }
    s
}
fn main() {
    let a: Vec[i64] = [1, 2, 3, 4].to_vec();
    let b: Vec[i64] = [10, 20].to_vec();
    println(f"{mix(a, a)}");
    println(f"{mix(a, b)}");
}
"#;

/// The counter moves between the compare and the index.
const WRITTEN_AFTER: &str = r#"fn next_sum(v: ref Vec[i64]) -> i64 {
    let n = v.len();
    let mut i = 0;
    let mut s = 0;
    while i < n {
        i = i + 1;
        s = s + v[i];
    }
    s
}
fn main() {
    let v: Vec[i64] = [1, 2, 3].to_vec();
    println(f"{next_sum(v)}");
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-guard-bounds-{name}-{}", std::process::id()));
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
/// runtime archive links, with the IR of the build at `ll`.
fn both(d: &Path, src: &str, env: &[(&str, &str)]) -> (Output, Option<Output>) {
    let file = d.join("main.kara");
    std::fs::write(&file, src).unwrap();
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

#[test]
fn mir_guard_bounds_proved_index_checks_only_its_sign() {
    for (env, proved) in [
        (&[][..], true),
        (&[("KARAC_MIR_GUARD_BOUNDS", "0")][..], false),
    ] {
        let d = dir(if proved { "expand" } else { "expand-off" });
        let (interp, built) = both(&d, EXPAND, env);
        assert_eq!(out(&interp), "0 7\n");
        let Some(built) = built else { return };
        assert_eq!(out(&built), "0 7\n");
        let f = function(&d.join("main.ll"), "expand");
        assert!(!f.is_empty(), "no expand in the IR");
        let sign = f.iter().filter(|l| l.contains("icmp sge i64")).count();
        let full = f.iter().filter(|l| l.contains("icmp ult i64")).count();
        // The guard's own `lo >= 0` is one `sge`. Proved, `chars[hi]`
        // checks its sign, and `chars[lo]` keeps `<u len`.
        if proved {
            assert_eq!((sign, full), (2, 1), "{}", f.join("\n"));
        } else {
            assert_eq!((sign, full), (1, 2), "{}", f.join("\n"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Each of these must still panic at its out-of-range index: the first
/// through the sign check its guard leaves, the others through the full
/// check they keep.
#[test]
fn mir_guard_bounds_uncovered_indexes_still_panic() {
    for (name, src, f, check, before) in [
        ("negative", NEGATIVE, "sum", "icmp sge i64", "6\n"),
        ("other-vec", OTHER_VEC, "mix", "icmp ult i64", "10\n"),
        (
            "written-after",
            WRITTEN_AFTER,
            "next_sum",
            "icmp ult i64",
            "",
        ),
    ] {
        let d = dir(name);
        let (interp, built) = both(&d, src, &[]);
        if built.is_some() {
            let body = function(&d.join("main.ll"), f);
            let checks: Vec<&String> = body
                .iter()
                .filter(|l| l.contains("icmp sge i64") || l.contains("icmp ult i64"))
                .collect();
            assert!(
                checks.len() == 1 && checks[0].contains(check),
                "{name}: {}",
                body.join("\n")
            );
        }
        let runs = std::iter::once(("interp", interp)).chain(built.map(|b| ("built", b)));
        for (how, o) in runs {
            let err = String::from_utf8_lossy(&o.stderr);
            assert_eq!(out(&o), before, "{name} {how}: {err}");
            assert_eq!(o.status.code(), Some(101), "{name} {how}: {err}");
            assert!(err.contains("index out of bounds"), "{name} {how}: {err}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
