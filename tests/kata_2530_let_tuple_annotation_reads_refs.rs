//! Kata 2530's counts arm: `let (top, many): (i64, i64) =
//! counts.last().unwrap();` over a `SortedMap[i64, i64]`, then
//! `counts.remove(top)`. The annotation reads the two `ref i64`s as
//! `i64`s (core-semantics.md §5.10), as `let pair: (i64, i64) = ..`
//! already did, but the tuple pattern bound the references and the
//! removal was refused as a write while `top` held a borrow of the map
//! (B-2026-11-23-69).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2530-{name}-{}", std::process::id()));
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

const LAST: &str = "fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m[3] = 1;
    m[5] = 2;
    let (a, b): (i64, i64) = m.last().unwrap();
    m.remove(a);
    m[a + 4] = b;
    let pair: (i64, i64) = m.last().unwrap();
    m.remove(pair.0);
    println(f\"{a} {b} {pair} {m.len()}\");
}
";

const UNANNOTATED: &str = "fn main() {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m[3] = 1;
    let (a, b) = m.last().unwrap();
    m.remove(a);
    println(f\"{a} {b}\");
}
";

#[test]
fn an_annotated_tuple_pattern_copies_the_refs_it_reads() {
    let f = program("last", LAST);
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "5 2 (9, 2) 1\n");
}

#[test]
fn an_unannotated_tuple_pattern_still_binds_the_refs() {
    let f = program("unannotated", UNANNOTATED);
    let (ok, out) = karac(&["check"], &f);
    assert!(
        !ok,
        "check accepted a write while `a` borrows the map: {out}"
    );
    assert!(out.contains("E0516"), "{out}");
}
