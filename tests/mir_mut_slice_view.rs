//! Writes through a range of a `Vec` of literals passed for a `mut Slice`
//! parameter (`mut w[2..4]`, `mut w[r]`, `mut w[3..=3]`) and through a
//! tuple-held `v.as_slice_mut()` reach the `Vec` in a build with
//! `KARAC_AUTO_PAR=0`, the bench lane's, as with auto-par on. Each view was
//! lowered on a shared borrow, so `literal_tables` made the `Vec` a
//! read-only static and the writes were lost (B-2026-11-09-82, the same
//! cause as B-2026-11-10-2, fixed in ba29c00d5c). The pins: the interpreter
//! and the built binary with auto-par off and on.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const VIEWS: &str = r#"fn poke(xs: mut Slice[i64]) {
    xs[0] = 99;
}
fn main() {
    let mut w: Vec[i64] = [1, 2, 3, 4];
    poke(mut w[2..4]);
    let r = 0..2;
    poke(mut w[r]);
    poke(mut w[3..=3]);
    println(f"{w}");
    let mut v: Vec[i64] = [1, 2, 3];
    let mut t: (mut Slice[i64], i64) = (v.as_slice_mut(), 5);
    t.0[0] = 9;
    println(f"{v}");
    for x in 0..2 {
        println(f"{x}");
    }
}
"#;

const WANT: &str = "[99, 2, 99, 99]\n[9, 2, 3]\n0\n1\n";

#[test]
fn mir_mut_slice_view_writes_reach_a_literal_vec() {
    let d = std::env::temp_dir().join(format!("karac-mut-slice-view-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let file = d.join("main.kara");
    std::fs::write(&file, VIEWS).unwrap();
    let file = file.to_str().unwrap();
    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", file])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&interp.stdout),
        WANT,
        "{}",
        String::from_utf8_lossy(&interp.stderr)
    );
    for par in ["0", "1"] {
        let exe = d.join(format!("main{par}"));
        let built = Command::new(env!("CARGO_BIN_EXE_karac"))
            .env("KARAC_AUTO_PAR", par)
            .args(["build", file, "-o", exe.to_str().unwrap()])
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&built.stderr).to_string();
        if common::link_or_skip(if built.status.success() {
            Ok(())
        } else {
            Err(why)
        })
        .is_none()
        {
            return;
        }
        let ran = Command::new(&exe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            WANT,
            "KARAC_AUTO_PAR={par}"
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}
