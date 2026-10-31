//! Binary-search midpoints (B-2026-10-28-41): the backend states `lo <= mid
//! < hi` after a midpoint under a `lo < hi` guard, so LLVM folds `nums[mid]`'s
//! bounds check. A wrong fact is `assume(false)`, and the compiled program
//! then runs on undefined behaviour (B-2026-08-30-6 was `(lo + hi) / 2` on a
//! negative range). So every spelling below searches signed ranges on both
//! sides of zero and unsigned ones, and must print what the MIR interpreter
//! prints and what a Python mirror of `SRC` printed (`WANT`); and a search
//! whose `hi` runs past the end must still stop at the bounds check, which
//! `mid < hi` alone does not fold.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"fn canonical(lo0: i64, hi0: i64, t: i64) -> i64 {
    let mut lo = lo0;
    let mut hi = hi0;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if mid < t { lo = mid + 1; } else { hi = mid; }
    }
    lo
}

fn shifted(lo0: i64, hi0: i64, t: i64) -> i64 {
    let mut lo = lo0;
    let mut hi = hi0;
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if mid < t { lo = mid + 1; } else { hi = mid; }
    }
    lo
}

fn halved(lo0: i64, hi0: i64, t: i64) -> i64 {
    let mut lo = lo0;
    let mut hi = hi0;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if mid < t { lo = mid + 1; } else { hi = mid; }
    }
    lo
}

fn unsigned(n: u64, t: u64) -> u64 {
    let mut lo: u64 = 0u64;
    let mut hi: u64 = n;
    while lo < hi {
        let mid = (lo + hi) / 2u64;
        if mid < t { lo = mid + 1u64; } else { hi = mid; }
    }
    lo
}

fn lower_bound(nums: ref Vec[i64], t: i64) -> i64 {
    let mut lo = 0;
    let mut hi = nums.len() as i64;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if nums[mid as usize] < t { lo = mid + 1; } else { hi = mid; }
    }
    lo
}

fn main() {
    let mut sum = 0;
    let mut lo0 = -6;
    while lo0 <= 6 {
        let mut hi0 = lo0;
        while hi0 <= 6 {
            let mut t = lo0 - 1;
            while t <= hi0 + 1 {
                let a = canonical(lo0, hi0, t);
                let b = shifted(lo0, hi0, t);
                // Truncating `(lo + hi) / 2` can land on `hi` below zero
                // and never shrink the range, so it runs on `0..` only.
                let c = if lo0 >= 0 { halved(lo0, hi0, t) } else { a };
                if a != b or a != c {
                    println(f"differ {lo0} {hi0} {t}: {a} {b} {c}");
                }
                sum = (sum * 31 + a + 100) % 1000003;
                t = t + 1;
            }
            hi0 = hi0 + 1;
        }
        lo0 = lo0 + 1;
    }
    println(f"signed {sum}");
    let mut usum: u64 = 0u64;
    let mut n: u64 = 0u64;
    while n < 20u64 {
        let mut t: u64 = 0u64;
        while t <= n + 1u64 {
            usum = usum + unsigned(n, t) * 3u64 + t;
            t = t + 1u64;
        }
        n = n + 1u64;
    }
    println(f"unsigned {usum}");
    let mut v = Vec.new();
    let mut i = 0;
    while i < 50 {
        v.push(i * 3 - 40);
        i = i + 1;
    }
    let mut found = 0;
    let mut t = -45;
    while t < 115 {
        found = found + lower_bound(v, t);
        t = t + 1;
    }
    println(f"vec {found}");
}
"#;

const PAST_END: &str = r#"fn search(nums: ref Vec[i64], t: i64) -> i64 {
    let mut lo = 0;
    let mut hi = nums.len() as i64 + 3;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if nums[mid as usize] < t { lo = mid + 1; } else { hi = mid; }
    }
    lo
}

fn main() {
    let mut v = Vec.new();
    v.push(1);
    v.push(2);
    println(search(v, 99));
}
"#;

/// The midpoints the backend stated in `module`: one `icmp` each.
fn stated(module: &str) -> usize {
    module
        .lines()
        .filter(|l| l.contains("%mid.lt.hi") && l.contains("= icmp"))
        .count()
}

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The compiled run's (success, stdout, stderr), or `None` when the
/// runtime archive is missing.
fn compiled(src: &str, tag: &str) -> Option<(bool, String, String)> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!("karac-mir-midpoint-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

const WANT: &str = "signed 726736\nunsigned 6100\nvec 4025\n";

#[test]
fn mir_midpoint_searches_match_on_both_backends() {
    let module = ir(SRC);
    // `canonical`, `shifted`, `unsigned` and `lower_bound` state one;
    // `halved`'s signed `(lo + hi) / 2` does not.
    assert_eq!(stated(&module), 4, "{module}");
    let interp = karac::mir::lower::run_source(SRC).expect("MIR interpreter");
    assert_eq!(interp.output, WANT);
    if let Some((ok, out, err)) = compiled(SRC, "searches") {
        assert!(ok, "{err}");
        assert_eq!(out, WANT);
    }
}

#[test]
fn mir_midpoint_past_the_end_still_panics() {
    assert_eq!(stated(&ir(PAST_END)), 1);
    if let Some((ok, out, err)) = compiled(PAST_END, "past-end") {
        assert!(!ok, "ran past the end: {out}");
        assert!(err.contains("out of bounds"), "{err}");
    }
}
