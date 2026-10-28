//! B-2026-10-24-2: an ungated `sort_by` range is split by one fused pass
//! around a median-of-three pivot (the less elements first, the rest back
//! to front, read from the end at the next level), and counted instead
//! when nothing is less than the pivot. The interpreter mirrors it call
//! for call, so a comparator that counts its calls sees the same number on
//! both backends, and the result is sorted and stable on every shape.
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
        let dir = std::env::temp_dir().join(format!("karac-sort-fused-{}-{n}", std::process::id()));
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

/// Each line ends `<misordered pairs> <comparator calls>`.
fn agree_and_sorted(src: &str, lines: usize) {
    let want = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(want.lines().count(), lines, "{want}");
    for line in want.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        assert_eq!(f[f.len() - 2], "0", "unsorted or unstable: {line}");
    }
    for opt in [0, 2] {
        if let Some(out) = build_and_run(src, opt) {
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

const CHECK: &str = r#"fn misordered(v: ref Vec[(i64, i64)]) -> i64 {
    let mut bad = 0;
    let mut j = 1;
    while j < v.len() {
        let (p, q) = (v[j - 1], v[j]);
        if p.0 > q.0 or (p.0 == q.0 and p.1 > q.1) { bad += 1; }
        j += 1;
    }
    bad
}
"#;

/// Distinct keys mixed with many copies of a small one: deep ranges whose
/// pivot is their least key, where the fused pass finds nothing less and
/// the range is counted from the start.
#[test]
fn mir_sort_fused_split_counts_a_range_whose_pivot_is_least() {
    let src = format!(
        "{CHECK}{}",
        r#"fn main() {
    for shape in [0, 1, 2, 3] {
        let n = 20000;
        let mut v: Vec[(i64, i64)] = Vec.new();
        let mut s = 7 + shape;
        let mut i = 0;
        while i < n {
            s = (s * 1103515245 + 12345) & 2147483647;
            let r = s / 16 + 1;
            let k = if shape == 0 { if i % 3 == 0 { 0 } else { r } } else if shape == 1 { if i % 5 < 3 { r % 4 } else { r } } else if shape == 2 { if r % 4 == 0 { 0 } else { r } } else { if r % 2 == 0 { r % 8 } else { r } };
            v.push((k, i));
            i += 1;
        }
        let mut calls = 0;
        v.sort_by(|x, y| { calls += 1; x.0.cmp(y.0) });
        println(f"{shape} {misordered(v)} {calls}");
    }
}
"#
    );
    agree_and_sorted(&src, 4);
}

/// Shuffled, cyclic, descending, nearly sorted and strided keys over few
/// and many distinct values, by `sort_by` and `sort_by_key`.
#[test]
fn mir_sort_fused_split_is_stable_on_every_shape() {
    let src = format!(
        "{CHECK}{}",
        r#"fn make(n: i64, seed: i64, m: i64, shape: i64) -> Vec[(i64, i64)] {
    let mut v: Vec[(i64, i64)] = Vec.new();
    let mut s = seed;
    let mut i = 0;
    while i < n {
        s = (s * 1103515245 + 12345) & 2147483647;
        let r = (s / 65536) % m;
        let k = if shape == 0 { r } else if shape == 1 { i % m } else if shape == 2 { n - i } else if shape == 3 { if i % 97 == 0 { r } else { i } } else { (i * 7919) % m };
        v.push((k, i));
        i += 1;
    }
    v
}
fn main() {
    for n in [65, 1000, 30000] {
        for m in [2, 1000, 1000000000] {
            for shape in [0, 1, 2, 3, 4] {
                let mut a = make(n, n + m + shape, m, shape);
                let mut calls = 0;
                a.sort_by(|x, y| { calls += 1; x.0.cmp(y.0) });
                let mut b = make(n, n + m + shape + 1, m, shape);
                b.sort_by_key(|x| x.0);
                println(f"{n} {m} {shape} {misordered(a) + misordered(b)} {calls}");
            }
        }
    }
}
"#
    );
    agree_and_sorted(&src, 45);
}
