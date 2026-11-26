//! B-2026-11-26-73: integer `Vector[T, N]` lane arithmetic wraps at the lane
//! width (deferred.md § Portable SIMD, "Integer lane arithmetic WRAPS"), in
//! the interpreter as in compiled code. The four corpus entries under
//! `corpus/fixtures/interpreter/numerics/vector_*wrap*` pin `+`, `-` and `*`;
//! this pins the rest of the list the spec gives: `/` and `%`, whose one
//! overflowing pair is `MIN / -1`, and a compound assignment.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-vector-lane-wrap-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const LANES: &str = "fn main() {
    let a: Vector[i8, 4] = Vector[i8, 4].splat(-128);
    let m: Vector[i8, 4] = Vector[i8, 4].splat(-1);
    let d = a / m;
    let r = a % m;
    println(f\"{d[0] as i64} {r[3] as i64}\");
    let mut s: Vector[u8, 4] = Vector[u8, 4].splat(250);
    s += Vector[u8, 4].splat(10);
    println(s[1] as i64);
    let x: Vector[u32, 4] = Vector[u32, 4].splat(4000000000);
    let y = x * Vector[u32, 4].splat(3);
    println(y[2] as i64);
}
";

// Worked by hand: -128 / -1 is 128, which wraps to -128 in `i8`, and the
// remainder is 0; 250 + 10 is 260, 4 in `u8`; 12000000000 mod 2^32 is
// 3410065408.
const LANES_EXPECTED: &str = "-128 0\n4\n3410065408\n";

#[test]
fn vector_lane_division_and_compound_assignment_wrap_interpreted() {
    let file = program("interp", LANES);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, LANES_EXPECTED);
}

// B-2026-11-26-87: compiled, `-128 / -1` and `-128 % -1` on an `i8` lane
// were LLVM's undefined `sdiv` / `srem` overflow and printed garbage
// (`784 1` run, `0 0` built); a signed lane now divides by 1 there.
#[cfg(feature = "llvm")]
#[test]
fn vector_lane_division_and_compound_assignment_wrap_compiled() {
    let file = program("llvm", LANES);
    let (ok, out) = karac(&["__mir-llvm-run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, LANES_EXPECTED);
}

// A lane divided by zero panics `div_by_zero`, as scalar division does
// (deferred.md § Portable SIMD, B-2026-11-26-87), with `/` and with `%`,
// and the lanes before it print nothing.
#[cfg(feature = "llvm")]
#[test]
fn a_vector_lane_divided_by_zero_panics_compiled() {
    for op in ["/", "%"] {
        let src = format!(
            "fn main() {{\n    let a: Vector[i32, 4] = Vector[i32, 4].splat(7);\n    let mut z: Vector[i32, 4] = Vector[i32, 4].splat(2);\n    z[2] = 0;\n    let d = a {op} z;\n    println(d[0] as i64);\n}}\n"
        );
        let file = program(if op == "/" { "zdiv" } else { "zrem" }, &src);
        let (ok, out) = karac(&["__mir-llvm-run", file.to_str().unwrap()]);
        assert!(!ok, "{op}: {out}");
        assert!(out.contains("division by zero"), "{op}: {out}");
        assert!(
            !out.starts_with('3') && !out.starts_with('1'),
            "{op}: {out}"
        );
    }
}

#[test]
fn scalar_overflow_still_traps_beside_a_wrapping_vector() {
    let file = program(
        "scalar",
        "fn main() {\n    let v: Vector[u8, 4] = Vector[u8, 4].splat(200);\n    let w = v + v;\n    println(w[0] as i64);\n    let b: u8 = 200;\n    let c = b + b;\n    println(c);\n}\n",
    );
    let (ok, out) = karac(&["__mir-run", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(out.starts_with("144\n"), "{out}");
    assert!(out.contains("overflow"), "{out}");
}
