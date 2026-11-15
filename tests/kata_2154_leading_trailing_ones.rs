//! Kata 2154: `trailing_ones` and `leading_ones`, which Rust has on every
//! integer, were refused with "no method 'trailing_ones' on type 'i64'",
//! though the four other bit counts were there. They return `i64`, as the
//! others do (docs/library/core-types.md "Bit counts"), and count within
//! the receiver's width; the values are checked against Rust's own.

use std::path::Path;
use std::process::{Command, Stdio};

const SRC: &str = r#"
fn main() {
    let a: i8 = -1;
    let b: i8 = 7;
    let c: u8 = 0b1110_0111;
    let d: i16 = -8;
    let e: u32 = 0;
    let f: i64 = 0b1011;
    let g: i64 = -1;
    let h: u64 = 18446744073709551614u64;
    let i: i128 = -4;
    let j: u128 = 340282366920938463463374607431768211455u128;
    println(f"{a.trailing_ones()} {a.leading_ones()}");
    println(f"{b.trailing_ones()} {b.leading_ones()}");
    println(f"{c.trailing_ones()} {c.leading_ones()}");
    println(f"{d.trailing_ones()} {d.leading_ones()}");
    println(f"{e.trailing_ones()} {e.leading_ones()}");
    println(f"{f.trailing_ones()} {f.leading_ones()}");
    println(f"{g.trailing_ones()} {g.leading_ones()}");
    println(f"{h.trailing_ones()} {h.leading_ones()}");
    println(f"{i.trailing_ones()} {i.leading_ones()}");
    println(f"{j.trailing_ones()} {j.leading_ones()}");
    let total: i64 = f.trailing_ones() + f.leading_ones();
    println(total);
}
"#;

fn expected() -> String {
    let rows: [(u32, u32); 10] = [
        ((-1i8).trailing_ones(), (-1i8).leading_ones()),
        (7i8.trailing_ones(), 7i8.leading_ones()),
        (0b1110_0111u8.trailing_ones(), 0b1110_0111u8.leading_ones()),
        ((-8i16).trailing_ones(), (-8i16).leading_ones()),
        (0u32.trailing_ones(), 0u32.leading_ones()),
        (0b1011i64.trailing_ones(), 0b1011i64.leading_ones()),
        ((-1i64).trailing_ones(), (-1i64).leading_ones()),
        (
            (u64::MAX - 1).trailing_ones(),
            (u64::MAX - 1).leading_ones(),
        ),
        ((-4i128).trailing_ones(), (-4i128).leading_ones()),
        (u128::MAX.trailing_ones(), u128::MAX.leading_ones()),
    ];
    let mut s: String = rows.iter().map(|(t, l)| format!("{t} {l}\n")).collect();
    s.push_str("2\n");
    s
}

fn run(lane: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2154-{lane}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([lane, "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn kata_2154_ones_counts_in_the_interpreter() {
    let (ok, out) = run("__mir-run");
    assert!(ok, "{out}");
    assert_eq!(out, expected());
}

#[cfg(feature = "llvm")]
#[test]
fn kata_2154_ones_counts_compiled() {
    let (ok, out) = run("__mir-llvm-run");
    if out.contains("runtime archive") {
        return;
    }
    assert!(ok, "{out}");
    assert_eq!(out, expected());
}
