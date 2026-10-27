//! A narrower number passed for a borrowed wider one (`v.contains(b)` with
//! `b: u8` on a `Vec[f64]`) is widened before it is lent, as it is when it
//! is moved (design.md §5, lossless widening is implicit at an argument),
//! on the MIR interpreter and on the built binary. B-2026-10-22-36.
#![cfg(feature = "llvm")]

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = r#"
fn has(v: Vec[i64], x: i64) -> bool {
    v.contains(x)
}

fn main() {
    let b = 200u8;
    let n = -5i8;
    let w = 7i32;
    let mut f: Vec[f64] = Vec.new();
    f.push(b);
    f.push(n);
    println(f"{f.contains(b)} {f.contains(n)} {f.contains(b + 1)}");
    let mut g: Vec[f32] = Vec.new();
    g.push(1.5);
    let h = 1.5f32;
    let mut d: Vec[f64] = Vec.new();
    d.push(h);
    println(f"{d.contains(h)} {g.contains(h)}");
    let i: Vec[i64] = vec![7, 8];
    println(f"{i.contains(w)} {has(i, w)} {i.contains(b)}");
}
"#;

const OUT: &str = "true true false\ntrue true\ntrue true false\n";

fn source(dir: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-widen-lent-{dir}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    (dir, file)
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_HASH_SEED", "1");
    c
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn mir_a_narrower_number_lent_for_a_wider_one_is_widened_first() {
    let (dir, file) = source("interp");
    let checked = karac()
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{}", text(&checked));
    let out = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), OUT);

    let exe = dir.join("main");
    let built = karac()
        .args([
            "build",
            "--mir",
            file.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), OUT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
