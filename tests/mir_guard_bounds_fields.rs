//! B-2026-11-14-28: an index into a field of a borrowed receiver or
//! parameter (`self.pairs[i]` in a `ref self` method) under a guard that
//! bounds it by that field's length checks only its sign on mir-llvm, as
//! one into a borrowed `Vec` already did (B-2026-10-28-33), including
//! through an `a < n and b < m` guard, whose flag the second compare's
//! arm sets. Kata 1570's merge went from 1.20G to 1.05G instructions.
//! The pins: the merge runs as before and its IR has no `<u len` check;
//! with the pass off it has two. A `mut ref self` whose loop shrinks the
//! field, an index into another field than the guard's, and a negative
//! start each still panic.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MERGE: &str = r#"struct Sparse { pairs: Vec[(i64, i64)] }
impl Sparse {
    fn dot(ref self, other: Sparse) -> i64 {
        let mut i = 0;
        let mut j = 0;
        let mut total = 0;
        while i < self.pairs.len() and j < other.pairs.len() {
            let (a, x) = self.pairs[i];
            let (b, y) = other.pairs[j];
            if a == b {
                total += x * y;
                i += 1;
                j += 1;
            } else if a < b {
                i += 1;
            } else {
                j += 1;
            }
        }
        total
    }
}
fn main() {
    let u = Sparse { pairs: [(0, 2), (3, 4), (7, 1)].to_vec() };
    let v = Sparse { pairs: [(3, 5), (7, 6), (9, 9)].to_vec() };
    println(u.dot(v));
}
"#;

/// The loop pops two of what the guard measured before indexing it.
const SHRINKS: &str = r#"struct Bag { v: Vec[i64] }
impl Bag {
    fn drain_sum(mut ref self) -> i64 {
        let mut i = 0;
        let mut s = 0;
        while i < self.v.len() {
            self.v.pop();
            self.v.pop();
            s += self.v[i];
            i += 1;
        }
        s
    }
}
fn main() {
    let mut b = Bag { v: [1, 2, 3, 4].to_vec() };
    println(b.drain_sum());
}
"#;

/// The guard bounds `a`; the index is into the shorter `b`.
const OTHER_FIELD: &str = r#"struct Two { a: Vec[i64], b: Vec[i64] }
impl Two {
    fn mix(ref self) -> i64 {
        let mut i = 0;
        let mut s = 0;
        while i < self.a.len() {
            s += self.b[i];
            i += 1;
        }
        s
    }
}
fn main() {
    let t = Two { a: [1, 2, 3].to_vec(), b: [10, 20].to_vec() };
    println(t.mix());
}
"#;

/// A negative start reaches `self.v[-1]` under `i < len`.
const NEGATIVE: &str = r#"struct Bag { v: Vec[i64] }
impl Bag {
    fn sum_from(ref self, k: i64) -> i64 {
        let mut i = k;
        let mut s = 0;
        while i < self.v.len() {
            s += self.v[i];
            i += 1;
        }
        s
    }
}
fn main() {
    let b = Bag { v: [1, 2, 3].to_vec() };
    println(b.sum_from(0));
    println(b.sum_from(-1));
}
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-guard-fields-{name}-{}", std::process::id()));
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
        .filter(|l| !l.trim_start().starts_with("%mono.fact"))
        .map(str::to_string)
        .collect()
}

#[test]
fn mir_guard_fields_merge_checks_only_signs() {
    for (env, proved) in [
        (&[][..], true),
        (&[("KARAC_MIR_GUARD_BOUNDS", "0")][..], false),
    ] {
        let d = dir(if proved { "merge" } else { "merge-off" });
        let (interp, built) = both(&d, MERGE, env);
        assert_eq!(out(&interp), "26\n");
        let Some(built) = built else { return };
        assert_eq!(out(&built), "26\n");
        let f = function(&d.join("main.ll"), "Sparse.dot");
        assert!(!f.is_empty(), "no Sparse.dot in the IR");
        let full = f.iter().filter(|l| l.contains("icmp ult i64")).count();
        assert_eq!(full, if proved { 0 } else { 2 }, "{}", f.join("\n"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// Each of these must still panic at its out-of-range index.
#[test]
fn mir_guard_fields_uncovered_indexes_still_panic() {
    for (name, src, f, before) in [
        ("shrinks", SHRINKS, "Bag.drain_sum", ""),
        ("other-field", OTHER_FIELD, "Two.mix", ""),
        ("negative", NEGATIVE, "Bag.sum_from", "6\n"),
    ] {
        let d = dir(name);
        let (interp, built) = both(&d, src, &[]);
        assert!(!interp.status.success(), "{name}: the interpreter ran on");
        assert_eq!(out(&interp), before, "{name} interpreted");
        let Some(built) = built else { continue };
        assert!(!built.status.success(), "{name}: the build ran on");
        assert_eq!(out(&built), before, "{name} built");
        let err = String::from_utf8_lossy(&built.stderr);
        assert!(err.contains("out of bounds"), "{name}: {err}");
        let ir = function(&d.join("main.ll"), f);
        assert!(!ir.is_empty(), "{name}: no {f} in the IR");
        let _ = std::fs::remove_dir_all(&d);
    }
}
