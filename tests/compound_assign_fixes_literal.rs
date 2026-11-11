//! A compound assignment into a target whose type is fixed is an
//! assignment to that target, so an open literal operand takes the target's
//! type (design.md §8 Literals, the kata 2103 ruling): `let bit = 4;
//! rods[i] |= bit` over a `u8` array makes `bit` a `u8`, as `rods[i] = bit`
//! would. It was refused as narrowing an `i64`, and `karac fix` declared
//! `bit: u8`. Where the target is the open binding (`let mut b = 0; b += y`)
//! the operator rule still applies, and `b` keeps `i64`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-compound-literal-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

fn runs(tag: &str, src: &str, expected: &str) {
    let path = temp(tag, src);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "{tag} {lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{tag} {lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn kata_2103_or_assigns_a_match_of_literals_into_a_u8_rod() {
    runs("kata-2103", KATA_2103, "5\n1.5\n7\n1\n1\n");
}

#[test]
fn an_open_compound_target_keeps_the_operator_rule() {
    runs("open-target", OPEN_TARGET, "5\n36342608838809702\n36\n");
}

#[test]
fn a_literal_out_of_range_for_the_target_is_refused() {
    let path = temp(
        "out-of-range",
        "fn main() {\n    let b = 300;\n    let mut x: u8 = 1;\n    x |= b;\n    println(x);\n}\n",
    );
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && stderr.contains("integer literal 300 out of range for 'u8'"),
        "{stderr}"
    );
}

const KATA_2103: &str = r#"fn count_points(rings: Str) -> i32 {
    let bytes = rings.bytes();
    let mut rods = [0u8; 10];
    let mut i = 0;
    while i + 1 < bytes.len() {
        let bit = match bytes[i] {
            b'R' => 1,
            b'G' => 2,
            _ => 4,
        };
        let r = (bytes[i + 1] - b'0') as usize;
        rods[r] |= bit;
        i += 2;
    }
    let mut n = 0;
    for r in rods {
        if r == 7 {
            n += 1;
        }
    }
    n
}

fn main() {
    let b = 4;
    let mut x: u8 = 1;
    x |= b;
    let f = 0.5;
    let mut y: f32 = 1.0;
    y += f;
    let k = 3;
    let mut s: i16 = 10;
    s -= k;
    println(x);
    println(y);
    println(s);
    println(count_points("B0B6G0R6R0R6G9"));
    println(count_points("B0R0G0R9R0B0G0"));
}
"#;

const OPEN_TARGET: &str = r#"fn main() {
    let mut acc = 0;
    let y: i32 = 5;
    acc += y;
    let z: i64 = acc;
    println(z);
    let mut h = 2166136261;
    let byte: u8 = 7;
    h = (h ^ byte) * 16777619;
    println(h);
    let t = 2;
    let mut w: u8 = 9;
    w <<= t;
    println(w);
}
"#;
