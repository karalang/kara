//! A borrowing iterator handed a closure by reference (`it.find(..)`,
//! `stream.next_if(..)`) cannot keep that reference: its type has no place
//! for a closure. MIR borrowck took the `mut ref` receiver to store every
//! other argument's borrows (core-semantics §9.5), the reference to the
//! closure local included, so a `while let` over `it.find(|x| x < 5)`
//! wrote the next iteration's closure while it was "borrowed" and was
//! refused E0516 (B-2026-11-10-24). The receiver now keeps only what the
//! closure holds. A slot whose type can hold a closure still keeps the
//! reference, and what a closure hands back still flows into the slot.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-closure-into-iterator-slot-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn refused(tag: &str, src: &str) -> String {
    let path = temp(tag, src);
    let out = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success() && stderr.contains("E0516"),
        "{tag}: {stderr}"
    );
    stderr
}

const PROGRAM: &str = r#"fn main() {
    let v = [1, 2, 3, 7, 8].to_vec();
    let mut it = v.iter();
    let mut small = 0;
    while let Some(x) = it.find(|x| x < 5) {
        small += x;
    }
    println(f"{small}");
    let mut it = v.iter();
    let mut found = 0;
    for _ in 0..3 {
        let x = it.find(|x| x > 2);
        if let Some(n) = x {
            found += n;
        }
    }
    println(found);
    let w = [[1, 5].to_vec(), [2, 6].to_vec(), [9, 1].to_vec()].to_vec();
    let mut stream = w.iter().peekable();
    let mut taken = 0;
    while let Some(item) = stream.next_if(|item| item[0] <= 2) {
        taken += item[1];
    }
    println(taken);
    let limit = 4;
    let mut it2 = v.iter();
    let mut s = 0;
    while let Some(x) = it2.find(|x| x < limit) {
        s += x;
    }
    println(s);
}
"#;

const EXPECTED: &str = "6\n18\n11\n6\n";

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

#[test]
fn a_borrowing_iterator_does_not_keep_the_closure_it_is_handed() {
    let path = temp("run", PROGRAM);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "{lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_slot_that_can_hold_a_closure_still_keeps_the_reference() {
    // `Vec[ref F]` with `F` the closure: the loop's next closure would
    // overwrite one `keep` still refers to.
    refused("holds-closure", PROGRAM_HOLDS_CLOSURE);
}

#[test]
fn what_a_closure_hands_back_still_flows_into_the_slot() {
    // The closure returns its captured `ref n`, which `fill` pushes: `keep`
    // outlives `n`, whatever happens to the closure itself.
    let err = refused("hands-back", PROGRAM_HANDS_BACK);
    assert!(err.contains("8:17:"), "{err}");
}

const PROGRAM_HOLDS_CLOSURE: &str = r#"fn put[F](slot: mut ref Vec[ref F], f: ref F) {
    slot.push(f);
}

fn main() {
    let mut keep = Vec.new();
    for i in 0..2 {
        let c = |x: i64| x + i;
        put(mut keep, c);
    }
    println(keep.len());
}
"#;

const PROGRAM_HANDS_BACK: &str = r#"fn fill(slot: mut ref Vec[ref i64], f: own MutFn() -> ref i64) {
    slot.push(f());
}

fn main() {
    let mut keep: Vec[ref i64] = Vec.new();
    for i in 0..2 {
        let n = i * 10;
        fill(mut keep, || ref n);
    }
    println(keep.len());
}
"#;
