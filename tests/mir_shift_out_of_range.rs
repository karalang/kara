//! A shift amount that is negative or at least the operand's width panics
//! with "shift amount out of range", not "integer overflow" (design.md §5,
//! B-2026-11-04-74), in the MIR interpreter and in a built binary. A shift
//! by the width minus one is legal whatever it does to the sign bit.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

/// Each program reads its amount from a call, so nothing folds it.
const CASES: &[(&str, &str)] = &[
    (
        "shl_width",
        "fn amount() -> i32 { 32 }\nfn main() {\n    let x: i32 = 1;\n    println(\"before\");\n    println(x << amount());\n}\n",
    ),
    (
        "shl_negative",
        "fn amount() -> i64 { -1 }\nfn main() {\n    let x: i64 = 1;\n    println(\"before\");\n    println(x << amount());\n}\n",
    ),
    (
        "shr_width",
        "fn amount() -> u8 { 8 }\nfn main() {\n    let x: u8 = 200;\n    println(\"before\");\n    println(x >> amount());\n}\n",
    ),
    (
        "shl_assign",
        "fn amount() -> i64 { 64 }\nfn main() {\n    let mut x: i64 = 3;\n    println(\"before\");\n    x <<= amount();\n    println(x);\n}\n",
    ),
];

/// `x << 31` on an `i32` sets the sign bit and is legal.
const LEGAL: &str = "fn amount() -> i32 { 31 }\nfn main() {\n    let x: i32 = 1;\n    println(x << amount());\n    let y: i32 = -8;\n    println(y >> amount());\n}\n";

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-shift-range-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.env("KARAC_AUTO_PAR", "0");
    c
}

fn write(name: &str, src: &str) -> (PathBuf, PathBuf) {
    let d = dir(name);
    let file = d.join("main.kara");
    std::fs::write(&file, src).unwrap();
    (d, file)
}

fn says_out_of_range(name: &str, how: &str, o: &Output) {
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{name} {how}: ran to the end: {o:?}");
    assert_eq!(stdout, "before\n", "{name} {how}: {o:?}");
    assert!(
        stderr.contains("shift amount out of range") && !stderr.contains("integer overflow"),
        "{name} {how}: {stderr}"
    );
}

#[test]
fn mir_shift_out_of_range_interp() {
    for (name, src) in CASES {
        let (d, file) = write(name, src);
        let o = karac()
            .args(["__mir-run", file.to_str().unwrap()])
            .output()
            .unwrap();
        says_out_of_range(name, "__mir-run", &o);
        let _ = std::fs::remove_dir_all(&d);
    }
    let (d, file) = write("legal", LEGAL);
    let o = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "-2147483648\n-1\n",
        "{o:?}"
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(feature = "llvm")]
#[test]
fn mir_shift_out_of_range_built() {
    for (name, src) in CASES.iter().copied().chain([("legal", LEGAL)]) {
        let (d, file) = write(&format!("built-{name}"), src);
        let exe = d.join("main");
        let built = karac()
            .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
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
        let o = Command::new(&exe).output().unwrap();
        if name == "legal" {
            assert_eq!(
                String::from_utf8_lossy(&o.stdout),
                "-2147483648\n-1\n",
                "{o:?}"
            );
        } else {
            says_out_of_range(name, "build", &o);
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
