//! `f"{n}"` of a single integer on the compiled backend: an inline shim
//! that counts the digits and writes them into a `String` from `malloc`
//! (`src/backend/mir/call.rs`, B-2026-10-14-39). Each width's edge values
//! print as on the MIR interpreter.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"
fn main() {
    let xs: Vec[i64] = [0, 1, -1, 9, 10, -10, 99, 100, 12345, -12345, 9223372036854775807, -9223372036854775807 - 1];
    for x in xs {
        let s = f"{x}";
        println(f"{s} {s.len()}");
    }
    let us: Vec[u64] = [0u64, 7u64, 10u64, 18446744073709551615u64];
    for u in us {
        println(f"{u}");
    }
    let a: i8 = -128i8;
    let b: u8 = 255u8;
    let c: i32 = -2147483648i32;
    let d: u32 = 4294967295u32;
    println(f"{a}");
    println(f"{b}");
    println(f"{c}");
    println(f"{d}");
    let mut total = 0;
    for i in 0..1000 {
        let s = f"{i - 500}";
        total += s.len();
    }
    println(f"{total}");
}
"#;

const WANT: &str = "0 1
1 1
-1 2
9 1
10 2
-10 3
99 2
100 3
12345 5
-12345 6
9223372036854775807 19
-9223372036854775808 20
0
7
10
18446744073709551615
-128
255
-2147483648
4294967295
3282
";

#[test]
fn mir_string_of_int_edges_match_on_both_backends() {
    let dir = std::env::temp_dir().join(format!("karac-mir-string-of-int-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();

    let interp = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        interp.status.success(),
        "interpreter: {}",
        String::from_utf8_lossy(&interp.stderr)
    );
    assert_eq!(String::from_utf8(interp.stdout).unwrap(), WANT);

    let exe = dir.join("main");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", file.to_str().unwrap(), "-o"])
        .arg(&exe)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    let linked = common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    });
    if linked.is_some() {
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap(), WANT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
