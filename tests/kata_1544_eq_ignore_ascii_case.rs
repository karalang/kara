//! Kata 1544's fold arm compared two letters as Rust does,
//! `top.eq_ignore_ascii_case(c)`, and the checker said only "no method
//! 'eq_ignore_ascii_case' on type 'char'". `eq_ignore_ascii_case(other) ->
//! bool` now exists on `char` and `u8` on every backend, with Rust's
//! meaning: equal once both are put in ASCII lower case. B-2026-11-14-9.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, backend: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1544-{tag}-{backend}-{}.kara",
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

fn runs(tag: &str, src: &str, want: &str) {
    for b in backends() {
        let out = karac(tag, b, src);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).as_ref()
            ),
            (Some(0), want),
            "{tag} on {b}: {stderr}"
        );
    }
}

/// The pairs below 256 that Rust calls equal ignoring ASCII case, counted
/// and folded into a checksum over `(i, j)`.
fn rust_pairs() -> (u64, u64) {
    let (mut count, mut check) = (0u64, 0u64);
    for i in 0u8..=255 {
        for j in 0u8..=255 {
            if i.eq_ignore_ascii_case(&j) {
                assert!((i as char).eq_ignore_ascii_case(&(j as char)), "{i} {j}");
                count += 1;
                check = (check * 31 + i as u64 * 256 + j as u64) % 1_000_000_007;
            }
        }
    }
    (count, check)
}

#[test]
fn kata_1544_char_eq_ignore_ascii_case_matches_rust() {
    let (count, check) = rust_pairs();
    let src = "fn main() {\n    let mut count = 0;\n    let mut check = 0;\n    for i in 0..256 {\n        for j in 0..256 {\n            let a = (i as u8) as char;\n            let b = (j as u8) as char;\n            if a.eq_ignore_ascii_case(b) {\n                count += 1;\n                check = (check * 31 + i * 256 + j) % 1000000007;\n            }\n        }\n    }\n    println(f\"{count} {check}\");\n    println(f\"{'é'.eq_ignore_ascii_case('É')} {'é'.eq_ignore_ascii_case('é')}\");\n}\n";
    runs("char", src, &format!("{count} {check}\nfalse true\n"));
}

#[test]
fn kata_1544_u8_eq_ignore_ascii_case_matches_rust() {
    let (count, check) = rust_pairs();
    let src = "fn main() {\n    let mut count = 0;\n    let mut check = 0;\n    for i in 0..256 {\n        for j in 0..256 {\n            let a = i as u8;\n            if a.eq_ignore_ascii_case(j as u8) {\n                count += 1;\n                check = (check * 31 + i * 256 + j) % 1000000007;\n            }\n        }\n    }\n    println(f\"{count} {check}\");\n    let b: u8 = 65;\n    println(f\"{b.eq_ignore_ascii_case(97)} {b.eq_ignore_ascii_case(98)}\");\n}\n";
    runs("u8", src, &format!("{count} {check}\ntrue false\n"));
}

#[test]
fn kata_1544_eq_ignore_ascii_case_checks_its_argument() {
    let src = "fn main() {\n    let c = 'a';\n    let n: i64 = 65;\n    println(f\"{c.eq_ignore_ascii_case(1)}\");\n    println(f\"{n.eq_ignore_ascii_case(97)}\");\n    println(f\"{c.eq_ignore_ascii_case()}\");\n}\n";
    let out = karac("check", "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let all = format!("{stdout}{stderr}");
    assert_ne!(out.status.code(), Some(0), "{all}");
    assert!(
        all.contains("eq_ignore_ascii_case expects an argument of type `char`, got `i64`"),
        "{all}"
    );
    assert!(
        all.contains("no method 'eq_ignore_ascii_case' on type 'i64'"),
        "{all}"
    );
    assert!(
        all.contains("eq_ignore_ascii_case expects 1 argument, got 0"),
        "{all}"
    );
}

#[test]
fn kata_1544_u8_to_ascii_case_matches_rust() {
    let mut check = 0u64;
    for i in 0u8..=255 {
        check = (check * 31 + i.to_ascii_lowercase() as u64) % 1_000_000_007;
        check = (check * 31 + i.to_ascii_uppercase() as u64) % 1_000_000_007;
    }
    let src = "fn main() {\n    let mut check = 0;\n    for i in 0..256 {\n        let b = i as u8;\n        let lo: u8 = b.to_ascii_lowercase();\n        let up: u8 = b.to_ascii_uppercase();\n        check = (check * 31 + lo as i64) % 1000000007;\n        check = (check * 31 + up as i64) % 1000000007;\n    }\n    println(f\"{check}\");\n    println(f\"{b'Q'.to_ascii_lowercase() as char}{b'q'.to_ascii_uppercase() as char}\");\n}\n";
    runs("u8case", src, &format!("{check}\nqQ\n"));
}
