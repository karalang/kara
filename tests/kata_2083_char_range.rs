//! Kata 2083's letters arm counted each letter with Rust's
//! `('a'..='z').map(..)`, and the checker refused it: the library's ranges
//! step through any type with `RangeStep`, which only the integers had.
//! `char` now has it, stepping over the surrogate gap `0xD800..=0xDFFF` as
//! Rust's `Step for char` does. A `for` loop straight over a char range
//! was lowered as a counting loop, which the MIR interpreter refused
//! ("Add on Char") and which would walk into the surrogates; it now
//! iterates the library's range on every backend.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-2083-{tag}-{backend}-{}.kara",
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

fn prints(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn kata_2083_a_char_range_is_an_iterator() {
    let src = "fn main() {\n    let parts: Vec[String] = ('a'..='e').map(|c| c.to_string()).collect();\n    println(parts.join(\"\"));\n    let back: Vec[char] = ('a'..'d').rev().collect();\n    let n = ('\\u{D7FE}'..='\\u{E001}').count();\n    let v: Vec[i64] = ('\\u{D7FE}'..='\\u{E001}').map(|c| c as i64).collect();\n    println(f\"{back} {n} {v}\");\n    let w: Vec[char] = ('a'..='z').step_by(5).collect();\n    let r: Vec[char] = ('a'..='z').step_by(5).rev().collect();\n    println(f\"{w} {r}\");\n}\n";
    prints(
        "iter",
        src,
        "abcde\n[c, b, a] 4 [55294, 55295, 57344, 57345]\n[a, f, k, p, u, z] [z, u, p, k, f, a]\n",
    );
}

#[test]
fn kata_2083_a_for_loop_over_a_char_range() {
    let src = "fn main() {\n    let mut t = String.new();\n    for c in 'x'..='z' {\n        t.push(c);\n    }\n    for c in 'a'..'d' {\n        t.push(c);\n    }\n    let lo = 'p';\n    for c in lo..='r' {\n        t.push(c);\n    }\n    let mut codes: Vec[i64] = Vec.new();\n    for c in '\\u{D7FE}'..='\\u{E001}' {\n        codes.push(c as i64);\n    }\n    let mut total = 0;\n    for i in 0..5 {\n        total += i;\n    }\n    println(f\"{t} {codes} {total}\");\n}\n";
    prints("loop", src, "xyzabcpqr [55294, 55295, 57344, 57345] 10\n");
}
