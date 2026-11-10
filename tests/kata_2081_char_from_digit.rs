//! Kata 2081's string arm wrote each base-`k` digit as Rust does,
//! `char::from_digit(d, k)`, the inverse of `c.to_digit(radix)`, and the
//! checker said only "no associated function 'from_digit' on type 'char'".
//! `char.from_digit(num: u32, radix: u32) -> Option[char]` now exists on
//! every backend: `0`..`9` then `a`..`z` below the radix, `None` from the
//! radix up, and a panic for a radix outside 2..=36.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-2081-{tag}-{backend}-{}.kara",
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
fn kata_2081_char_from_digit_matches_every_radix() {
    // Every digit below 40 against every radix: 665 chars, and the checksum
    // Python's digit string gives.
    let src = "fn main() {\n    let mut some = 0;\n    let mut check = 0;\n    for radix in 2..37 {\n        for n in 0..40 {\n            match char.from_digit(n as u32, radix as u32) {\n                Some(c) => {\n                    some += 1;\n                    check = (check * 31 + c as i64 + radix) % 1000000007;\n                }\n                None => check = (check * 31 + 1) % 1000000007,\n            }\n        }\n    }\n    println(f\"{some} {check}\");\n    let big: u32 = 4000000000;\n    println(f\"{char.from_digit(7, 8)} {char.from_digit(8, 8)} {char.from_digit(35, 36)} {char.from_digit(big, 10)}\");\n}\n";
    runs(
        "digits",
        src,
        0,
        "665 702253020\nSome(7) None Some(z) None\n",
        "",
    );
}

#[test]
fn kata_2081_char_from_digit_radix_out_of_range_panics() {
    let src = "fn main() {\n    let r = 37;\n    println(char.from_digit(1, r as u32));\n}\n";
    runs(
        "radix",
        src,
        101,
        "",
        "char.from_digit: the radix is not in 2..=36",
    );
}

#[test]
fn kata_2081_char_from_digit_takes_u32s() {
    let src = "fn main() {\n    let d = 3;\n    println(char.from_digit(d, 10));\n    println(char.from_digit(1));\n}\n";
    let out = karac("types", "check", src);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains(
            "char.from_digit expects a digit of type `u32`, got `i64` (cast with `as u32`)"
        ),
        "{text}"
    );
    assert!(
        text.contains("char.from_digit expects 2 arguments, a digit and a radix, got 1"),
        "{text}"
    );
}
