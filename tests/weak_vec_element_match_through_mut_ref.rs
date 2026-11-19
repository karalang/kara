//! B-2026-11-19-54: since B-2026-11-17-152 (b3388ea406) an element
//! reached through `mut ref`s is a `mut ref` scrutinee, indexed for
//! writing. An element of a `Vec[weak N]` is no such place: reading it
//! upgrades it to an `Option[N]` value (core semantics §6.5). Matching
//! `w[0]` on a `mut ref Vec[weak N]` took the raw weak slot and the MIR
//! interpreter stopped at "discriminant of a value that is not an
//! initialized enum". Found by three corpus programs on mir-interp.

use std::process::Command;

const SRC: &str = "shared struct N { mut v: i64 }
fn fill(w: mut ref Vec[weak N]) {
    let a: N = N { v: 7 };
    w.push(a);
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 1); } }
}
fn main() {
    let mut w: Vec[weak N] = Vec.new();
    fill(mut w);
    match w[0] { Some(x) => { println(x.v); } None => { println(0 - 2); } }
}
";

fn run(lane: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-b1954-{lane}-{}.kara", std::process::id()));
    std::fs::write(&path, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(lane)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{lane}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn weak_vec_element_matched_through_a_mut_ref_upgrades() {
    assert_eq!(run("__mir-run"), "7\n-2\n");
    #[cfg(feature = "llvm")]
    assert_eq!(run("__mir-llvm-run"), "7\n-2\n");
}
