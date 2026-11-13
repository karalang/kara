//! B-2026-11-13-16 — `s == a` with a `Slice` on one side and an `Array` of
//! the same element type on the other read the array as a slice in
//! compiled code, so it was never equal there (and `!=` always held),
//! while the MIR interpreter compared them. A DHCP server's cookie check
//! `pkt[236..240] != COOKIE` dropped every datagram. Both sides are now
//! compared as slices.
//!
//! B-2026-11-13-17 — `a == s` with the `Array` (or `Vec`) on the left was
//! refused with an `.as_slice()` fix while `s == a` was accepted. The ruling
//! (design.md § Slices, Comparison): `==` and `!=` between a `Slice`, a
//! `Vec` and an `Array` of one element type compare elements in either
//! order, and a length mismatch is `false`.

mod common;

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn run(src: &str) -> String {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("karac-slice-array-eq-i-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file.to_str().unwrap()])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(feature = "llvm")]
fn compiled(src: &str) -> Option<String> {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir =
        std::env::temp_dir().join(format!("karac-slice-array-eq-b-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    let out = linked.map(|()| Command::new(&exe).current_dir(&dir).output().expect("run"));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    assert!(out.status.success(), "{:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

const SRC: &str = r#"const COOKIE: Array[u8, 4] = [99, 130, 83, 99];
fn eq(s: Slice[u8], a: Array[u8, 4]) -> bool { s == a }
fn main() {
    let mut pkt = Vec.filled(8, 0u8);
    pkt[4..8] = COOKIE;
    let a: Array[u8, 4] = [99, 130, 83, 99];
    let names: Vec[String] = ["a".to_string(), "b".to_string()].to_vec();
    let arr: Array[String, 2] = ["a".to_string(), "b".to_string()];
    let ints: Vec[i64] = [1, 2].to_vec();
    let pair: Array[i64, 2] = [1, 2];
    println(f"{pkt[4..8] == COOKIE} {pkt[4..8] != COOKIE} {eq(pkt[4..8], a)}");
    println(f"{pkt[0..4] == a} {pkt[4..7] == a} {names[..] == arr} {ints[0..2] == pair} {ints[0..1] == pair} {ints[0..2] == ints}");
}
"#;

const OUT: &str = "true false true\nfalse false true true false true\n";

#[test]
fn interp_compares_a_slice_with_an_array() {
    assert_eq!(run(SRC), OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_compares_a_slice_with_an_array() {
    if let Some(out) = compiled(SRC) {
        assert_eq!(out, OUT);
    }
}

const ORDERS: &str = r#"fn main() {
    let v: Vec[u8] = [1u8, 2].to_vec();
    let a: Array[u8, 2] = [1, 2];
    let b: Array[u8, 3] = [1, 2, 3];
    let s: Slice[u8] = v[0..2];
    let w: Vec[u8] = [1u8, 2, 3].to_vec();
    println(f"{s == a} {a == s} {s == v} {v == s} {v == a} {a == v}");
    println(f"{s != b} {b != s} {s == w} {w == s} {w == b} {b == w} {v == b} {b == v}");
    let n: Vec[String] = ["x".to_string()].to_vec();
    let na: Array[String, 1] = ["x".to_string()];
    println(f"{n == na} {na == n} {na == n[..]} {n[..] == na}");
}
"#;

const ORDERS_OUT: &str =
    "true true true true true true\ntrue true false false true true false false\ntrue true true true\n";

#[test]
fn interp_compares_slice_vec_and_array_in_either_order() {
    assert_eq!(run(ORDERS), ORDERS_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_compares_slice_vec_and_array_in_either_order() {
    if let Some(out) = compiled(ORDERS) {
        assert_eq!(out, ORDERS_OUT);
    }
}
