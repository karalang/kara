//! B-2026-11-24-42: `half.find(|c: char| c != 'a')` on `half = s[..n /
//! 2]`, Rust's `str::find` over a string's first half, got only "'find'
//! takes a `Str` pattern" with no fix: the `char_indices().find(..)` fix
//! (B-2026-11-20-140) reached only a `String` receiver with an unannotated
//! closure. A `Str` view gets it now, and a `c: char` annotation, which the
//! pair pattern the fix writes makes needless, is dropped by the rewrite.
//! Kata 1328's slices arm.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_1328_find_with_a_closure_on_a_str_view_is_fixed() {
    let src = "fn first_not_a(s: String) -> i64 {
    let n = s.len();
    let half = s[..n / 2];
    half.find(|c: char| c != 'a').unwrap_or(-1)
}

fn main() {
    let s = \"aabcaa\".to_string();
    let t = \"xyz\".to_string();
    let i = t.find(|c: char| c == 'z');
    println(f\"{first_not_a(s)} {first_not_a(\"aaaa\".to_string())} {i}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-kata-1328-find-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let after = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(!check.status.success(), "{err}");
    assert!(
        text.contains("half.char_indices().find(|(_, c)| c != 'a').map(|(i, _)| i).unwrap_or(-1)"),
        "{text}"
    );
    assert!(
        text.contains("t.char_indices().find(|(_, c)| c == 'z').map(|(i, _)| i);"),
        "{text}"
    );
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "2 -1 Some(2)\n");
}
