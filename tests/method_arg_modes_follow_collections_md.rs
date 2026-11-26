//! The argument modes collections.md gives a handful of collection methods
//! (B-2026-11-26-70): `union` / `intersection` / `difference` take
//! `other: ref Set[T]`, `Vec.cmp` and a derived `cmp` / `eq` /
//! `partial_cmp` take `other: ref Self`, the `SortedMap` ordered queries
//! (`floor` / `ceiling` / `lower` / `higher`) take `key: ref K`, and `fill`
//! takes `val: ref T` and clones it into each slot under `T: Clone`. A bare
//! parameter borrows (D5), so each of these was refused as giving one away
//! (E0200), and a local passed to `fill` was moved.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "#[derive(Eq, Ord, PartialEq, PartialOrd, Clone)]\nstruct P { n: i64 }\n";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-arg-modes-collections-md-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

#[test]
fn set_algebra_borrows_the_other_set() {
    runs_everywhere(
        "fn u(a: ref Set[i64], b: Set[i64]) -> i64 { a.union(b).len() + a.intersection(b).len() * 10 + a.difference(b).len() * 100 }\n\
         fn main() { let a = Set.from([1, 2, 3]); let b = Set.from([3, 4]); println(u(a, b)); println(b.len()); }\n",
        "214\n2\n",
    );
    runs_everywhere(
        "fn u(a: ref SortedSet[String], b: SortedSet[String]) -> SortedSet[String] { a.union(b) }\n\
         fn main() { let mut a = SortedSet.new(); a.insert(String.from(\"x\")); let mut b = SortedSet.new(); b.insert(String.from(\"y\")); \
         let c = u(a, b); println(f\"{c.len()} {b.len()}\"); }\n",
        "2 1\n",
    );
}

#[test]
fn cmp_and_eq_borrow_the_other_value() {
    runs_everywhere(
        "fn c(a: ref Vec[i64], b: Vec[i64]) -> bool { a.cmp(b) == Ordering.Less }\n\
         fn main() { let a = [1, 2]; let b = [1, 3]; println(c(a, b)); println(b[1]); }\n",
        "true\n3\n",
    );
    runs_everywhere(
        "fn c(a: ref P, b: P) -> bool { a.cmp(b) == Ordering.Greater and not a.eq(b) and a.partial_cmp(b) == Some(Ordering.Greater) }\n\
         fn main() { let a = P { n: 2 }; let b = P { n: 1 }; println(c(a, b)); println(b.n); }\n",
        "true\n1\n",
    );
}

#[test]
fn the_ordered_queries_borrow_the_key() {
    runs_everywhere(
        "fn q(m: ref SortedMap[String, i64], k: String) -> i64 { \
         let a = match m.floor(k) { Some((_, v)) => v, None => 0 }; \
         let b = match m.ceiling(k) { Some((_, v)) => v, None => 0 }; \
         let c = match m.lower(k) { Some((_, v)) => v, None => 0 }; \
         let d = match m.higher(k) { Some((_, v)) => v, None => 0 }; a * 1000 + b * 100 + c * 10 + d }\n\
         fn main() { let mut m = SortedMap.new(); m.insert(String.from(\"b\"), 2); m.insert(String.from(\"d\"), 4); \
         let k = String.from(\"c\"); println(q(m, k)); println(k); }\n",
        "2424\nc\n",
    );
}

#[test]
fn fill_clones_the_value_and_leaves_it() {
    runs_everywhere(
        "fn main() { let mut a = [String.from(\"a\"), String.from(\"b\")]; let z = String.from(\"z\"); \
         a.fill(z); println(f\"{a[0]} {a[1]} {z}\"); }\n",
        "z z z\n",
    );
    runs_everywhere(
        "fn f[T: Clone](x: mut ref Vec[T], v: T) { x.fill(v) }\n\
         fn main() { let mut a = [String.from(\"a\"), String.from(\"b\")]; let z = String.from(\"q\"); \
         f(mut a, z); println(f\"{a[0]}{a[1]} {z}\"); }\n",
        "qq q\n",
    );
    // A value the argument makes is cloned into the slots as before.
    runs_everywhere(
        "fn main() { let mut a = [String.from(\"a\")]; a.fill(String.from(\"n\")); println(a[0]); }\n",
        "n\n",
    );
}
