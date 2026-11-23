//! Kata 2519's iter arm: `let mut (at, n) = (i, 0)` with `i` an item of
//! `v.iter()`, a `ref i64`. A `mut` tuple pattern over a tuple literal
//! reads a lent `Copy` element into a counter, as `let mut at = i` does
//! (core-semantics.md §5.10); the MIR was invalid before (B-2026-11-23-67).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2519-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str], f: &PathBuf) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(f)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const COUNTERS: &str = "fn main() {
    let v: Vec[i64] = [3, 5].to_vec();
    for i in v.iter() {
        let mut (at, n) = (i, 0);
        while at > 0 {
            n += 1;
            at -= 1;
        }
        println(f\"{n} {at} {i}\");
    }
    let mut x = 7;
    let mut (r, m) = (mut ref x, 1);
    *r += m;
    println(f\"{x}\");
}
";

#[test]
fn a_mut_tuple_pattern_reads_a_lent_copy_element() {
    let f = program("counters", COUNTERS);
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "3 0 3\n5 0 5\n8\n");
}
