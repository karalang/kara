//! A checked `/` or `%` whose divisor is a native length, assigned once,
//! has no `MIN / -1` check on mir-llvm (B-2026-10-30-36): a length is
//! `>= 0`, and the zero check stays. Kata 74's `m[mid / cols][mid % cols]`
//! then divides once where the check's divisor select made it divide
//! twice: 6.86G -> 4.46G instructions on Linux (legacy 4.17G). The pins:
//! the binary search's IR has no select on its divisor, a user `len`
//! returning -1 still overflows, and a zero length still divides by zero.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SEARCH: &str = r#"fn search_matrix(m: ref Vec[Vec[i64]], target: i64) -> bool {
    let rows = m.len();
    if rows == 0 {
        return false;
    }
    let cols = m[0].len();
    if cols == 0 {
        return false;
    }
    let mut lo = 0;
    let mut hi = rows * cols - 1;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let v = m[mid / cols][mid % cols];
        if v == target {
            return true;
        } else if v < target {
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    false
}
fn main() {
    let mut m: Vec[Vec[i64]] = Vec.new();
    for i in 0..7 {
        let mut row: Vec[i64] = Vec.new();
        for j in 0..5 {
            row.push((i * 5 + j) * 2);
        }
        m.push(row);
    }
    let mut hits = 0;
    for t in 0..80 {
        if search_matrix(m, t) {
            hits = hits + 1;
        }
    }
    println(f"{hits}");
}
"#;

/// A user `len` is a body, not a native: its -1 still overflows.
const USER_LEN: &str = r#"struct Odd {
    k: i64,
}
impl Odd {
    fn len(self) -> i64 {
        self.k
    }
}
fn main() {
    let o = Odd { k: -1 };
    let n = o.len();
    let big = i64.MIN;
    println(f"{big / n}");
}
"#;

/// An empty vector's length is 0: the division by it still aborts.
const ZERO_LEN: &str = r#"fn main() {
    let v: Vec[i64] = Vec.new();
    let n = v.len();
    let x = 10;
    println(f"{x % n}");
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-length-div-{name}-{}", std::process::id()));
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
/// runtime archive links, with the build's IR at `main.ll`.
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
fn mir_length_divisor_divides_without_the_overflow_check() {
    for (env, on) in [
        (&[][..], true),
        (&[("KARAC_MIR_LENGTH_DIVISORS", "0")][..], false),
    ] {
        let d = dir(if on { "search" } else { "search-off" });
        let (interp, built) = both(&d, SEARCH, env);
        assert_eq!(out(&interp), "35\n");
        let Some(built) = built else { return };
        assert_eq!(out(&built), "35\n");
        let f = function(&d.join("main.ll"), "search_matrix");
        assert!(!f.is_empty(), "no search_matrix in the IR");
        // The check divides by 1 where it would overflow.
        let selects = f
            .iter()
            .filter(|l| l.contains("select i1") && l.contains(", i64 1, i64 %"))
            .count();
        assert_eq!(selects, if on { 0 } else { 2 }, "{}", f.join("\n"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn mir_length_divisor_keeps_what_a_length_does_not_prove() {
    for (name, src, msg) in [
        ("user-len", USER_LEN, "integer overflow"),
        ("zero-len", ZERO_LEN, "division by zero"),
    ] {
        let d = dir(name);
        let (interp, built) = both(&d, src, &[]);
        let runs = std::iter::once(("interp", interp)).chain(built.map(|b| ("built", b)));
        for (how, o) in runs {
            let err = String::from_utf8_lossy(&o.stderr);
            assert_eq!(out(&o), "", "{name} {how}: {err}");
            assert_eq!(o.status.code(), Some(101), "{name} {how}: {err}");
            assert!(err.contains(msg), "{name} {how}: {err}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
