//! B-2026-11-13-5 (design.md § Division, remainder and shifts;
//! library/core-types.md): `div_euclid` and `rem_euclid` exist on every
//! integer width and on `f32` / `f64`, as Rust's. They existed only on
//! `i64`. Pinned on the interpreter, a built binary and the ASan lane
//! against Python's floor division, with the 128-bit extremes and the
//! `MIN.rem_euclid(-1)` corner.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-euclid-widths-{name}-{}", std::process::id()));
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
fn euclid_division_on_every_width_and_both_floats() {
    every_lane("widths", r#"fn main() {
    let a: i8 = -17;
    println(f"{a.div_euclid(5)} {a.rem_euclid(5)} {a.div_euclid(-5)} {a.rem_euclid(-5)}");
    let b: i16 = -1000;
    println(f"{b.div_euclid(7)} {b.rem_euclid(7)}");
    let c: i32 = -17;
    println(f"{c.div_euclid(5)} {c.rem_euclid(5)} {c.div_euclid(-5)} {c.rem_euclid(-5)}");
    let d: i64 = -17;
    println(f"{d.div_euclid(5)} {d.rem_euclid(5)}");
    let e: i128 = -170000000000000000000;
    println(f"{e.div_euclid(3)} {e.rem_euclid(3)}");
    let f: u8 = 250;
    println(f"{f.div_euclid(7)} {f.rem_euclid(7)}");
    let g: u16 = 65000;
    println(f"{g.div_euclid(9)} {g.rem_euclid(9)}");
    let h: u32 = 17;
    println(f"{h.div_euclid(5)} {h.rem_euclid(5)}");
    let i: u64 = 18000000000000000000;
    println(f"{i.div_euclid(7)} {i.rem_euclid(7)}");
    let j: u128 = 17;
    println(f"{j.div_euclid(5)} {j.rem_euclid(5)}");
    let m: i8 = -128;
    println(m.rem_euclid(-1));
    let x: f64 = -7.5;
    println(f"{x.div_euclid(2.0)} {x.rem_euclid(2.0)} {x.div_euclid(-2.0)} {x.rem_euclid(-2.0)}");
    let y: f32 = -7.5;
    println(f"{y.div_euclid(2.0)} {y.rem_euclid(2.0)}");
}

"#, "-4 3 4 3\n-143 1\n-4 3 4 3\n-4 3\n-56666666666666666667 1\n35 5\n7222 2\n3 2\n2571428571428571428 4\n3 2\n0\n-4.0 0.5 4.0 0.5\n-4.0 0.5\n");
}

#[test]
fn euclid_division_at_the_128_bit_extremes() {
    every_lane(
        "big",
        r#"fn main() {
    let j: u128 = 340282366920938463463374607431768211455;
    println(f"{j.div_euclid(7)} {j.rem_euclid(7)}");
    let k: u64 = 18446744073709551615;
    println(f"{k.div_euclid(10)} {k.rem_euclid(10)}");
    let m: i128 = -170141183460469231731687303715884105728;
    println(m.rem_euclid(-1));
}
"#,
        "48611766702991209066196372490252601636 3\n1844674407370955161 5\n0\n",
    );
}
