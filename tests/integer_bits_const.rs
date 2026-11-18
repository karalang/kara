//! B-2026-11-18-9 (kata 1611): the integer types had `MAX` and `MIN` but not
//! Rust's `BITS`, so `i32.BITS - n.leading_zeros()` was refused with "no
//! associated constant `BITS` on `i32`". Every integer type now has it,
//! an `i64` as every count is in Kāra (core-types.md, Bit counts).

use std::process::{Command, Stdio};

const SRC: &str = "const WIDTH: i64 = u16.BITS;

fn main() {
    println(f\"{i8.BITS} {i16.BITS} {i32.BITS} {i64.BITS} {i128.BITS}\");
    println(f\"{u8.BITS} {u16.BITS} {u32.BITS} {u64.BITS} {u128.BITS}\");
    println(f\"{usize.BITS} {isize.BITS} {WIDTH}\");
    let n: i32 = 6;
    let total: i64 = i32.BITS - n.leading_zeros();
    println(f\"{total}\");
    let shift = (i32.BITS - 1) as u32;
    println(f\"{1 << shift}\");
}
";

const OUT: &str = "8 16 32 64 128\n8 16 32 64 128\n64 64 16\n3\n2147483648\n";

fn run(lane: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-int-bits-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("b.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "b.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn integer_bits_const_runs_in_the_interpreter() {
    let (out, err) = run("__mir-run");
    assert_eq!(out, OUT, "{err}");
}

#[test]
fn integer_bits_const_checks_clean() {
    let (_, err) = run("check");
    assert!(!err.contains("error"), "{err}");
}

#[cfg(feature = "llvm")]
#[test]
fn integer_bits_const_is_lowered_by_llvm() {
    let (out, err) = run("__mir-llvm-run");
    if err.contains("Linker failed") && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
    {
        eprintln!("skipping: no runtime archive");
        return;
    }
    assert_eq!(out, OUT, "{err}");
}
