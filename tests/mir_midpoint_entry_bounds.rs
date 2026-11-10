//! A binary search's `lo` only rises and its `hi` only falls (`lo = mid +
//! 1`, `hi = mid`), so the backend states them as one-way counters and,
//! at the midpoint, `lo0 <= mid < hi0` against their values on entry
//! (B-2026-11-10-37). LLVM 22 weakens the midpoint's own `mid >= lo` and,
//! in karac's pipeline, loses `hi <= hi0`, so kata 275's `citations[mid]`
//! kept its bounds check, which LLVM 18 folded: 1.51G instructions against
//! 1.32G with the facts. The pins: searches that must print what a Python
//! mirror printed (`WANT`), one whose `hi` starts past the end and must
//! still stop at the check, and the built `main` of a prefix search over
//! a 1024-element vector, whose two `v[mid]` checks fold only with the
//! facts (`KARAC_MIR_MIDPOINT_MONO=0` keeps it).
#![cfg(feature = "llvm")]

mod common;

use std::path::Path;
use std::process::Command;

const SRC: &str = r#"fn h_index_prefix(citations: ref Vec[i64], n: i64) -> i64 {
    let mut lo = 0;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if citations[mid] >= n - mid { hi = mid; } else { lo = mid + 1; }
    }
    return n - lo;
}

fn first_at_least(v: ref Vec[i64], from: i64, t: i64) -> i64 {
    let mut lo = from;
    let mut hi = v.len() as i64;
    while lo < hi {
        let mid = (lo + hi) >> 1;
        if v[mid] < t { lo = mid; lo = lo + 1; } else { hi = mid; }
    }
    lo
}

fn main() {
    let size = 1024;
    let mut citations: Vec[i64] = Vec.new();
    let mut state = 275;
    let mut cur = 0;
    let mut i = 0;
    while i < size {
        state = (state * 1103515245 + 12345) & 2147483647;
        cur = cur + (state / 256) % 3;
        citations.push(cur);
        i = i + 1;
    }
    let mut sink = 0;
    let mut q = 0;
    while q < 3000 {
        state = (state * 1103515245 + 12345) & 2147483647;
        let n = 1 + (state / 256) % size;
        sink = (sink * 131 + h_index_prefix(citations, n)) % 1000000007;
        q = q + 1;
    }
    println(sink);
    let mut found = 0;
    let mut t = -5;
    while t < 700 {
        found = found + first_at_least(citations, (t + 10) % 7, t);
        t = t + 1;
    }
    println(found);
}
"#;

const WANT: &str = "961067804\n242276\n";

const PAST_END: &str = r#"fn search(nums: ref Vec[i64], t: i64) -> i64 {
    let mut lo = 0;
    let mut hi = nums.len() as i64 + 3;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if nums[mid] < t { lo = mid + 1; } else { hi = mid; }
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

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("karac-mid-entry-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds `src` in `d` (with `KARAC_MIR_MIDPOINT_MONO=0` when `off`) and
/// returns the binary's output, exit success and the optimised module.
fn build(d: &Path, src: &str, off: bool) -> Option<(String, bool, String)> {
    let file = d.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let exe = d.join(if off { "off" } else { "on" });
    let ll = d.join(if off { "off.ll" } else { "on.ll" });
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    if off {
        c.env("KARAC_MIR_MIDPOINT_MONO", "0");
    }
    let built = c
        .env("KARAC_AUTO_PAR", "0")
        .env("KARAC_MIR_CARRY_STORE", "report")
        .env("KARAC_MIR_OPT_LL", &ll)
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    })?;
    let ran = Command::new(&exe).output().unwrap();
    let mut module = String::new();
    for p in [ll.clone(), d.join(format!("{}.0", ll.display()))] {
        if let Ok(m) = std::fs::read_to_string(p) {
            module.push_str(&m);
        }
    }
    Some((
        String::from_utf8_lossy(&ran.stdout).to_string(),
        ran.status.success(),
        module,
    ))
}

/// The bounds checks against the 1024-element vector left in `module`.
fn checks(module: &str) -> usize {
    module
        .lines()
        .filter(|l| {
            let l = l.trim_end();
            (l.contains("icmp ult i64") && l.ends_with(", 1024"))
                || (l.contains("icmp ugt i64") && l.ends_with(", 1023"))
        })
        .count()
}

#[test]
fn a_binary_search_states_its_bounds_on_entry() {
    let d = dir("src");
    let file = d.join("interp.kara");
    std::fs::write(&file, SRC).unwrap();
    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&interp.stdout), WANT);
    let Some((on, ok, module)) = build(&d, SRC, false) else {
        return;
    };
    assert!(ok, "the program failed");
    assert_eq!(on, WANT);
    let (off, _, off_module) = build(&d, SRC, true).unwrap();
    assert_eq!(off, WANT);
    // Both searches' checks: `h_index_prefix`'s and `first_at_least`'s.
    assert_eq!(
        checks(&off_module),
        2,
        "without the facts the checks should stay: the pin is void\n{off_module}"
    );
    assert_eq!(checks(&module), 0, "the check did not fold\n{module}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_search_past_the_end_still_stops_at_the_check() {
    let d = dir("past");
    let Some((out, ok, _)) = build(&d, PAST_END, false) else {
        return;
    };
    assert!(!ok, "the search past the end ran on: {out}");
    let _ = std::fs::remove_dir_all(&d);
}
