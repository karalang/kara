//! Two finds of kata 1058, each printed alike on every backend.
//!
//! - `c.to_digit(radix)` and `c.is_digit(radix)` of a char are computed in
//!   line for a radix in 2..=36 (they were a runtime call that matched the
//!   method's name among twenty, half of the kata's benchmark, B-2026-11-06-33); a radix
//!   outside that range still panics with the runtime's words.
//! - `Vec.rotate_left` and `Vec.rotate_right`, which the kata's
//!   differential reached for and the library did not have
//!   (B-2026-11-06-35).

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1058-{tag}-{backend}-{}.kara",
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
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

/// `src` prints `want` and exits `code` on every backend, with `err` in its
/// stderr.
fn runs(tag: &str, src: &str, code: i32, want: &str, err: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(
            (out.status.code(), stdout.as_ref()),
            (Some(code), want),
            "{tag} on {b}: {stderr}"
        );
        assert!(stderr.contains(err), "{tag} on {b}: {stderr}");
    }
}

#[test]
fn kata_1058_char_to_digit_in_line_matches_every_radix() {
    // Every code point below 700 against every radix: 1016 digits, and the
    // checksum Python's `int(c, radix)` over ASCII letters and digits gives.
    let src = "fn main() {\n    let mut check = 0;\n    let mut some = 0;\n    for code in 0..700 {\n        let c = char.try_from(code as u32).unwrap_or('?');\n        for radix in 2..37 {\n            match c.to_digit(radix as u32) {\n                Some(d) => {\n                    some += 1;\n                    check = (check * 31 + d as i64 * 7 + code + radix) % 1000000007;\n                }\n                None => check = (check * 31 + 1) % 1000000007,\n            }\n            if c.is_digit(radix as u32) {\n                check = (check * 37 + 3) % 1000000007;\n            }\n        }\n    }\n    println(f\"{some} {check}\");\n    println(f\"{'7'.to_digit(8)} {'8'.to_digit(8)} {'z'.to_digit(36)} {'Z'.to_digit(36)} {'é'.to_digit(16)}\");\n}\n";
    runs(
        "digits",
        src,
        0,
        "1016 158473880\nSome(7) None Some(35) Some(35) None\n",
        "",
    );
}

#[test]
fn kata_1058_char_to_digit_radix_out_of_range_panics() {
    let src = "fn main() {\n    let r = 40;\n    println('7'.to_digit(r as u32));\n}\n";
    runs(
        "radix",
        src,
        101,
        "",
        "char.to_digit: radix 40 is not in 2..=36",
    );
}

#[test]
fn kata_1058_vec_rotates() {
    let src = "fn main() {\n    let mut v = [1, 2, 3, 4, 5];\n    v.rotate_left(2);\n    println(v);\n    v.rotate_right(2);\n    println(v);\n    let mut w: Vec[String] = [\"a\", \"b\", \"c\"].iter().map(|s| s.to_string()).collect();\n    w.rotate_left(1);\n    println(w);\n    w.rotate_left(0);\n    w.rotate_right(3);\n    println(w);\n    let mut e: Vec[i64] = Vec.new();\n    e.rotate_left(0);\n    println(e);\n    v.rotate_left(6);\n}\n";
    runs(
        "rotate",
        src,
        101,
        "[3, 4, 5, 1, 2]\n[1, 2, 3, 4, 5]\n[b, c, a]\n[b, c, a]\n[]\n",
        "rotate_left: mid out of bounds",
    );
}
