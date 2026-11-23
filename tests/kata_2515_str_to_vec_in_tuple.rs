//! Kata 2515's harness: `["a", "b"].to_vec()` as a tuple element whose
//! slot is `Vec[Str]`. The slot names the views the literals are, as it
//! does at a `let` of the `Vec` alone (B-2026-11-23-66).

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-k2515-{name}-{}", std::process::id()));
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

const IN_A_TUPLE: &str = "fn main() {
    let x: (Vec[Str], i64) = ([\"a\", \"b\"].to_vec(), 1);
    let cases: Vec[(Vec[Str], i64)] = [([\"c\"].to_vec(), 2), ([\"d\", \"e\"].to_vec(), 3)].to_vec();
    println(f\"{x.0} {x.1} {cases.len()} {cases[1].0}\");
}
";

#[test]
fn a_literal_to_vec_takes_its_tuple_slots_str() {
    let f = program("in_a_tuple", IN_A_TUPLE);
    let (ok, out) = karac(&["check"], &f);
    assert!(ok, "check refused the tuple's `Vec[Str]` element: {out}");
    let (ok, out) = karac(&["run", "--interp"], &f);
    assert!(ok, "run: {out}");
    assert_eq!(out, "[a, b] 1 2 [d, e]\n");
}
