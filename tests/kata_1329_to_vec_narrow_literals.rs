//! B-2026-11-24-43: `let mat: Vec[Vec[i32]] = [[3, 3, 1, 1].to_vec(),
//! ..].to_vec();`, a list of integer literals turned `.to_vec()` as Rust
//! writes a `Vec`, was refused with "expected 'Vec[Vec[i32]]', found
//! 'Vec[Vec[i64]]'": the wanted element type reached a list behind
//! `.to_vec()` only when it held a `Str`, so the literals kept the default
//! `i64`. They now take the slot's number, as in a bare list literal, and a
//! value of another type is still refused, at the value. Kata 1329's
//! harness.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1329-{tag}-{backend}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(backend)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["check", "__mir-run", "__mir-llvm-run"]
    } else {
        &["check", "__mir-run"]
    }
}

#[test]
fn kata_1329_to_vec_of_literals_takes_the_wanted_number() {
    let src = "fn main() {
    let mat: Vec[Vec[i32]] = [[3, 3, 1, 1].to_vec(), [2, 2, 1, 2].to_vec()].to_vec();
    let a: Vec[u8] = [200, 9].to_vec();
    let f: Vec[f32] = [1.5, 2.25].to_vec();
    let t: Vec[(u8, i32)] = [(1, -2), (3, 4)].to_vec();
    let mut s: i32 = 0;
    for row in mat.iter() {
        for x in row.iter() {
            s += x;
        }
    }
    println(f\"{s} {a[0]} {f[1]} {t[1].0}\");
}
";
    for b in backends() {
        let out = karac("lits", b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(out.status.code(), Some(0), "{b}: {stdout}{stderr}");
        if *b != "check" {
            assert_eq!(stdout, "15 200 2.25 3\n", "{b}: {stderr}");
        }
    }
}

#[test]
fn kata_1329_to_vec_still_refuses_a_wider_value() {
    let src = "fn main() {
    let x: i64 = 7;
    let a: Vec[i32] = [x, 1].to_vec();
    println(f\"{a[0]}\");
}
";
    let out = karac("wide", "check", src);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(out.status.code(), Some(0), "{text}");
    assert!(text.contains(":3:24:"), "{text}");
}
