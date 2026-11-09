//! Kata 2035 (Partition Array Into Two Arrays to Minimize Sum Difference):
//! a natural bound for a sorted range is `i64.MAX`, passed straight to a
//! method that borrows it (`ss.range(lo, i64.MAX)`, `v.contains(i64.MAX)`)
//! or to a function that takes `ref T`. Lowering treated `i64.MAX` as a
//! place, a field of the local `i64`, and refused the program with "an
//! expression with no recorded type is not lowered yet". A primitive's
//! constant is a value: it is built in a temporary and the temporary lent.

use std::path::Path;
use std::process::{Command, Stdio};

fn mir_run(tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2035-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(Path::new(&dir));
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn kata_2035_int_limit_lent_to_a_sorted_range() {
    let (ok, out, err) = mir_run(
        "range",
        "fn main() {
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(5);
    s.insert(9);
    println(s.range(6, i64.MAX).count());
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(3, 30);
    println(m.range(i64.MIN, 4).count());
}",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "1\n1\n");
}

#[test]
fn kata_2035_int_limit_lent_to_contains() {
    let (ok, out, err) = mir_run(
        "contains",
        "fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(i64.MAX);
    println(v.contains(i64.MAX));
    println(v.contains(i64.MIN));
    let w: Vec[u8] = [1u8, 255u8].to_vec();
    println(w.contains(u8.MAX));
}",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "true\nfalse\ntrue\n");
}

#[test]
fn kata_2035_int_limit_lent_to_a_ref_parameter() {
    let (ok, out, err) = mir_run(
        "refparam",
        "fn g[T: Clone](a: ref T) -> T {
    a.clone()
}

fn h(a: ref i32) -> i32 {
    a / 2
}

fn main() {
    println(g(i64.MAX));
    println(h(i32.MIN));
}",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "9223372036854775807\n-1073741824\n");
}
