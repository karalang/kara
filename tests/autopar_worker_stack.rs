//! B-2026-11-14-7: an auto-par loop runs its body on pool workers, so a deep
//! recursion in the body ran on a worker's 8 MiB stack where the sequential
//! program has the main thread's 1 GiB. Kata 2127's struct arm (a 999998-deep
//! `tail` for the mutual pair at the end of the range) died of a bare SIGSEGV
//! whenever a worker took that iteration. A worker now gets the main thread's
//! depth, and an overflow on a worker is reported as one (exit 134 with the
//! stack-overflow note), as on the main thread.
#![cfg(all(feature = "llvm", unix))]

use std::path::{Path, PathBuf};
use std::process::Command;

const DEEP: &str = r#"struct Office { favored_by: Vec[Vec[i64]] }
impl Office {
    fn tail(ref self, person: i64, skip: i64) -> i64 {
        let mut best = 0;
        for q in self.favored_by[person as usize].iter() {
            if *q != skip {
                let d = 1 + self.tail(*q, skip);
                if d > best { best = d; }
            }
        }
        best
    }
}
fn invite(favorite: Vec[i64]) -> i64 {
    let n = favorite.len() as i64;
    let mut favored_by: Vec[Vec[i64]] = Vec.new();
    for _ in 0..n { favored_by.push(Vec.new()); }
    for i in 0..n { favored_by[favorite[i as usize] as usize].push(i); }
    let office = Office { favored_by };
    let mut pairs = 0;
    for a in 0..n {
        let b = favorite[a as usize];
        if a < b and favorite[b as usize] == a {
            pairs += 2 + office.tail(a, b) + office.tail(b, a);
        }
    }
    pairs
}
fn main() {
    let n: i64 = 1000000;
    let f: Vec[i64] = (0..n).map(|i| if i >= n - 2 { 2 * n - 3 - i } else { i + 1 }).collect();
    println(invite(f));
}
"#;

const OVERFLOW: &str = r#"fn down(x: i64) -> i64 { if x == 0 { 0 } else { 1 + down(x - 1) } }
fn main() {
    let n: i64 = 1000000;
    let big: i64 = 1 << 40;
    let mut s = 0;
    for i in 0..n {
        s += if i == n - 1 { down(big) } else { i % 7 };
    }
    println(s);
}
"#;

/// The program built from `src`, or `None` when no runtime archive links.
fn build(tag: &str, src: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("karac-worker-stack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "main.kara", "-o", "main.out"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr);
    if !built.status.success() {
        assert!(
            err.contains("Linker failed")
                && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none(),
            "does not build: {err}"
        );
        eprintln!("skipping the run: no runtime archive");
        return None;
    }
    Some(dir.join("main.out"))
}

fn run(exe: &Path) -> (String, Option<i32>, String) {
    let out = Command::new(exe)
        .env("KARAC_THREADS", "4")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Which worker takes the deep iteration varies, so each program runs
/// several times; before the fix the deep one failed in 4 of 4 runs here.
#[test]
fn a_deep_recursion_in_an_auto_par_body_fits_a_worker() {
    let Some(exe) = build("deep", DEEP) else {
        return;
    };
    for i in 0..4 {
        let (out, code, err) = run(&exe);
        assert_eq!(
            (out.as_str(), code),
            ("1000000\n", Some(0)),
            "run {i}: {err}"
        );
    }
}

#[test]
fn an_overflow_on_a_worker_is_reported_as_one() {
    let Some(exe) = build("overflow", OVERFLOW) else {
        return;
    };
    for i in 0..3 {
        let (_, code, err) = run(&exe);
        assert_eq!(code, Some(134), "run {i}: {err}");
        assert!(
            err.contains("fatal runtime error: stack overflow"),
            "run {i}: {err}"
        );
    }
}
