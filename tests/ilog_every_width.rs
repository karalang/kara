//! B-2026-11-21-58 (library/core-types.md, integer logarithms): `ilog2`,
//! `ilog10` and `ilog(base)`, the floor of the logarithm, and their
//! `checked_` forms, on every integer width, as Rust's, with the result an
//! `i64` as every count is in Kāra. Kata 1295 (numbers with an even count of
//! digits) reached for `n.ilog10()` and found `no method 'ilog10'`. Pinned
//! on the interpreter and, with the backend, on a built binary and the ASan
//! lane, against Rust's own `ilog`, at each width's extremes; and the two
//! panics, a value not positive and a base below 2.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("karac-ilog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn asan_available() -> bool {
    if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
        return false;
    }
    // One probe directory per test thread: two tests probing at once
    // would delete each other's.
    let dir = Scratch::new(&format!("asan-probe-{:?}", std::thread::current().id()));
    let c = dir.path().join("probe.c");
    let exe = dir.path().join("probe");
    std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
    Command::new("cc")
        .arg("-fsanitize=address")
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .status()
        .is_ok_and(|s| s.success())
        && Command::new(&exe).status().is_ok_and(|s| s.success())
}

/// The program's stdout on the interpreter, as a built binary and on the
/// ASan lane, each where it can run here.
fn every_lane(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();

    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{name}: interpreter: {err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{name}: interpreter"
    );

    if !cfg!(feature = "llvm") {
        return;
    }
    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "{name}: binary: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: binary");
    }

    if !asan_available() {
        eprintln!("{name}: skipping the ASan lane: no ASan-capable cc");
        return;
    }
    let out = Command::new(karac)
        .args(["__mir-llvm-asan", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.code() == Some(3) && err.contains("Linker failed") {
        let _ = common::link_or_skip(Err(err.into_owned()));
        return;
    }
    assert_eq!(out.status.code(), Some(0), "{name}: ASan:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}: ASan");
}

#[test]
fn ilog_on_every_width_and_sign() {
    every_lane("widths", r#"fn main() {
    let a: i8 = 127;
    println(f"{a.ilog2()} {a.ilog10()} {a.ilog(3)} {(1i8).ilog2()} {(1i8).ilog10()}");
    let b: i16 = 1000;
    println(f"{b.ilog2()} {b.ilog10()} {b.ilog(10)} {(999i16).ilog10()}");
    let c: i32 = 2147483647;
    println(f"{c.ilog2()} {c.ilog10()} {c.ilog(7)}");
    let d: i64 = 9223372036854775807;
    println(f"{d.ilog2()} {d.ilog10()} {d.ilog(1000)} {(100000i64).ilog10()} {(99999i64).ilog10()}");
    let e: i128 = 170141183460469231731687303715884105727;
    println(f"{e.ilog2()} {e.ilog10()} {e.ilog(3)}");
    let f: u8 = 255;
    println(f"{f.ilog2()} {f.ilog10()} {f.ilog(2)} {f.ilog(255)} {f.ilog(16)}");
    let g: u16 = 65535;
    println(f"{g.ilog2()} {g.ilog10()}");
    let h: u32 = 4294967295;
    println(f"{h.ilog2()} {h.ilog10()} {h.ilog(65536)}");
    let i: u64 = 18446744073709551615;
    println(f"{i.ilog2()} {i.ilog10()} {(10000000000000000000u64).ilog10()} {i.ilog(18446744073709551615)}");
    let j: u128 = 340282366920938463463374607431768211455;
    println(f"{j.ilog2()} {j.ilog10()} {j.ilog(256)}");
    let total: i64 = d.ilog10() + i.ilog2();
    println(total);
}
"#, "6 2 4 0 0\n9 3 3 2\n30 9 11\n62 18 6 5 4\n126 38 80\n7 2 7 1 1\n15 4\n31 9 1\n63 19 19 1\n127 38 15\n81\n");
}

#[test]
fn checked_ilog_gives_none_where_ilog_panics() {
    every_lane(
        "checked",
        r#"fn main() {
    let zero: i64 = "".len();
    let neg: i32 = -8;
    let u: u16 = 0;
    println(f"{zero.checked_ilog10()} {zero.checked_ilog2()} {zero.checked_ilog(3)}");
    println(f"{neg.checked_ilog2()} {neg.checked_ilog10()} {neg.checked_ilog(2)} {u.checked_ilog2()}");
    let n: i64 = 1000;
    println(f"{n.checked_ilog10()} {n.checked_ilog2()} {n.checked_ilog(1)} {n.checked_ilog(0)} {n.checked_ilog(-2)}");
    let m: u128 = 340282366920938463463374607431768211455;
    println(f"{m.checked_ilog10()} {m.checked_ilog(1)}");
    match (500i16).checked_ilog(5) {
        Some(k) => println(k + 1),
        None => println("none"),
    }
}
"#,
        "None None None\nNone None None None\nSome(3) Some(9) None None None\nSome(38) None\n4\n",
    );
}

/// The program fails on the interpreter and, with the backend, as a
/// built binary, its error naming `want`.
fn traps(name: &str, src: &str, want: &str) {
    let dir = Scratch::new(name);
    let file = dir.path().join("main.kara");
    std::fs::write(&file, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let f = file.to_str().unwrap();
    let out = Command::new(karac)
        .args(["__mir-run", f])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{name}: interpreter ran clean");
    assert!(err.contains(want), "{name}: interpreter: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    let exe = dir.path().join("main");
    let built = Command::new(karac)
        .args(["build", f, "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&built.stderr).to_string())
    });
    if linked.is_some() {
        let out = Command::new(&exe).stdin(Stdio::null()).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{name}: binary ran clean");
        assert!(err.contains(want), "{name}: binary: {err}");
    }
}

#[test]
fn ilog10_panics_on_zero() {
    traps(
        "zero",
        "fn main() {\n    let n: i64 = \"\".len();\n    println(n.ilog10());\n}\n",
        "argument of integer logarithm must be positive",
    );
}

#[test]
fn ilog2_panics_on_a_negative_value() {
    traps(
        "negative",
        "fn main() {\n    let n: i32 = \"ab\".len() as i32 - 5;\n    println(n.ilog2());\n}\n",
        "argument of integer logarithm must be positive",
    );
}

#[test]
fn ilog_panics_on_a_base_below_two() {
    traps(
        "base",
        "fn main() {\n    let n: u64 = 81;\n    let b: u64 = \"a\".len() as u64;\n    println(n.ilog(b));\n}\n",
        "base of integer logarithm must be at least 2",
    );
}

#[test]
fn ilog_takes_a_base_of_the_receiver_type() {
    let dir = Scratch::new("basety");
    let file = dir.path().join("main.kara");
    std::fs::write(
        &file,
        "fn main() {\n    let n: u64 = 81;\n    let b: i32 = 3;\n    println(n.ilog(b));\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", file.to_str().unwrap()])
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a base of another type checked clean"
    );
    assert!(all.contains("ilog expects a base of type `u64`"), "{all}");
}
