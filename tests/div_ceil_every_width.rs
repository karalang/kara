//! B-2026-11-21-56 (library/core-types.md, Euclidean division): `div_ceil`,
//! the quotient rounded up toward positive infinity, exists on every
//! integer width, as Rust's. Kata 1283 (smallest divisor under a
//! threshold) reached for it and found `no method 'div_ceil'`. Pinned on
//! the interpreter and, with the backend, on a built binary and the ASan
//! lane, against Python's `-(-a // b)`, with the 128-bit extremes; and its
//! two traps, a zero divisor and the signed `MIN.div_ceil(-1)`.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-div-ceil-{name}-{}", std::process::id()));
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
fn div_ceil_on_every_width_and_sign() {
    every_lane("widths", r#"fn main() {
    let a: i8 = -17;
    println(f"{a.div_ceil(5)} {a.div_ceil(-5)} {(17i8).div_ceil(5)} {(17i8).div_ceil(-5)}");
    let b: i16 = -1000;
    println(b.div_ceil(7));
    let c: i32 = 2147483647;
    println(c.div_ceil(2));
    let d: i64 = 10;
    println(f"{d.div_ceil(5)} {(0).div_ceil(5)} {d.div_ceil(3)}");
    let e: i128 = -170000000000000000000;
    println(e.div_ceil(3));
    let f: u8 = 250;
    println(f.div_ceil(7));
    let g: u16 = 65000;
    println(g.div_ceil(9));
    let h: u32 = 17;
    println(h.div_ceil(5));
    let i: u64 = 18000000000000000000;
    println(i.div_ceil(7));
    let j: u128 = 17;
    println(j.div_ceil(5));
    let m: i8 = -128;
    println(f"{m.div_ceil(1)} {(127i8).div_ceil(-1)}");
}
"#, "-3 4 4 -3\n-142\n1073741824\n2 0 4\n-56666666666666666666\n36\n7223\n4\n2571428571428571429\n4\n-128 -127\n");
}

#[test]
fn div_ceil_at_the_128_bit_extremes() {
    every_lane(
        "big",
        r#"fn main() {
    let j: u128 = 340282366920938463463374607431768211455;
    println(j.div_ceil(7));
    let k: u64 = 18446744073709551615;
    println(k.div_ceil(10));
    let m: i128 = -170141183460469231731687303715884105728;
    println(f"{m.div_ceil(1)} {m.div_ceil(-2)}");
    let n: i128 = 170141183460469231731687303715884105727;
    println(n.div_ceil(-1));
}
"#,
        "48611766702991209066196372490252601637\n1844674407370955162\n-170141183460469231731687303715884105728 85070591730234615865843651857942052864\n-170141183460469231731687303715884105727\n",
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
fn div_ceil_traps_on_a_zero_divisor() {
    traps(
        "zero",
        "fn main() {\n    let d: u32 = \"0\".len() as u32 - 1;\n    let x: u32 = 7;\n    println(x.div_ceil(d));\n}\n",
        "division by zero",
    );
}

#[test]
fn div_ceil_traps_on_the_signed_min_over_minus_one() {
    traps(
        "min",
        "fn main() {\n    let m: i8 = -128;\n    let d: i8 = \"\".len() as i8 - 1;\n    println(m.div_ceil(d));\n}\n",
        "overflow",
    );
}

#[test]
fn div_ceil_has_no_float_form() {
    let dir = Scratch::new("float");
    let file = dir.path().join("main.kara");
    std::fs::write(
        &file,
        "fn main() {\n    let x: f64 = 7.5;\n    println(x.div_ceil(2.0));\n}\n",
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
    assert!(!out.status.success(), "a float div_ceil checked clean");
    assert!(all.contains("div_ceil"), "{all}");
}
