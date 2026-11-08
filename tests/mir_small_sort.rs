//! Short sorts (at most `SMALL_SORT` = 20 elements) are one insertion sort
//! with no scratch buffer, in the MIR interpreter, the runtime and the
//! compiled merge sort alike (kata 56's bench sorts millions of short
//! vectors). The comparator schedule is unspecified (docs/library/
//! collections.md, 88642fa4dd), but the backends still agree on it, and a
//! comparator that is not a total order leaves a permutation of the input
//! and never panics.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("karac-small-sort-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn build_and_run(src: &str, opt: u8) -> Option<String> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8(out.stdout).expect("utf-8"))
}

const SMALL_SORTS: &str = r#"
fn make(n: i64, seed: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s = seed;
    let mut i = 0;
    while i < n {
        s = (s * 1103515245 + 12345) & 2147483647;
        v.push((s % 5, i));
        i += 1;
    }
    v
}
fn sorted_and_stable(v: ref Vec[(i64, i64)]) -> bool {
    let mut i = 1;
    while i < v.len() {
        let (a, b) = (v[i - 1], v[i]);
        if a.0 > b.0 or (a.0 == b.0 and a.1 > b.1) { return false; }
        i += 1;
    }
    true
}
fn permutation(v: ref Vec[(i64, i64)], n: i64) -> bool {
    let mut ids: Vec[i64] = v.iter().map(|p| p.1).collect();
    ids.sort();
    let mut i = 0;
    while i < n {
        if ids[i] != i { return false; }
        i += 1;
    }
    v.len() == n
}
fn main() {
    for n in [0, 1, 2, 3, 7, 19, 20, 21, 33] {
        let mut a = make(n, n + 11);
        a.sort_by(|x, y| x.0.cmp(y.0));
        let mut b = make(n, n + 12);
        b.sort_by_key(|x| x.0);
        let mut c = make(n, n + 13);
        let mut calls = 0;
        c.sort_by(|x, y| {
            calls += 1;
            if (x.1 * 7 + y.1 + calls) % 3 == 0 { Ordering.Less } else { Ordering.Greater }
        });
        let ids: Vec[i64] = c.iter().map(|p| p.1).collect();
        println(f"{n} {sorted_and_stable(a)} {sorted_and_stable(b)} {permutation(c, n)} {calls} {ids}");
    }
}
"#;

#[test]
fn small_sorts_agree_and_survive_a_non_total_comparator() {
    let want = karac::mir::lower::run_source(SMALL_SORTS)
        .expect("interp")
        .output;
    for line in want.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        assert_eq!(&f[1..4], ["true", "true", "true"], "{line}");
    }
    // Twenty elements or fewer is one insertion sort: at most n(n-1)/2 calls.
    let twenty = want.lines().find(|l| l.starts_with("20 ")).expect("n=20");
    let calls: usize = twenty.split(' ').nth(4).unwrap().parse().unwrap();
    assert!(calls <= 190, "{twenty}");
    for opt in [0, 2] {
        if let Some(out) = build_and_run(SMALL_SORTS, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}
