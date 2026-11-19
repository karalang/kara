//! Kata 2268's Rust template keeps its letter counts in `vec![0; 26]`,
//! which `karac fix` spells `Vec.filled(26, 0)`, and prices each count
//! by `for (i, c) in cnt.iter().enumerate() { ans += c * (i as i32 / 9 + 1); }`
//! with `ans: i32`. Rust infers the counts `i32` from that
//! line; in Kāra the fill's literal kept the default `i64` (design.md §8
//! Literals) and the line was a narrowing whose only advice was `as
//! i32`, though written `cnt[i] * ..` it was offered the fill's suffix.
//! A loop's element is now traced back to the fill as an index is
//! (B-2026-11-18-82).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

fn fixed_to_the_suffix(name: &str, the_loop: &str, want: &str) {
    let src = format!(
        "fn presses(s: Str) -> i32 {{
    let mut cnt = Vec.filled(26, 0);
    for c in s.bytes() {{
        cnt[(c - b'a') as i64] += 1;
    }}
    let mut ans: i32 = 0;
{the_loop}
    ans
}}

fn main() {{
    println(presses(\"abcabca\"));
}}
"
    );
    let file = write(name, &src);
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("Vec.filled(26, 0i32)"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, want);
}

#[test]
fn kata_2268_enumerated_element_gets_the_fill_suffix() {
    fixed_to_the_suffix(
        "k2268-enumerate",
        "    for (i, c) in cnt.iter().enumerate() {
        ans += c * (i as i32 / 9 + 1);
    }",
        "7\n",
    );
}

/// A loop over the counts themselves, or over `cnt.iter()`, reads each
/// element into `ans` as it is: the fill's literal is still open there
/// and takes `i32` from that use, so no suffix is
/// needed and `karac check` passes the program as written.
fn needs_no_suffix(name: &str, the_loop: &str, want: &str) {
    let src = format!(
        "fn presses(s: Str) -> i32 {{
    let mut cnt = Vec.filled(26, 0);
    for c in s.bytes() {{
        cnt[c - b'a'] += 1;
    }}
    let mut ans: i32 = 0;
{the_loop}
    ans
}}

fn main() {{
    println(presses(\"abcabca\"));
}}
"
    );
    let file = write(name, &src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(&src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, want);
}

#[test]
fn kata_2268_iterated_element_needs_no_suffix() {
    needs_no_suffix(
        "k2268-iter",
        "    for c in cnt.iter() {
        ans += c * 2;
    }",
        "14\n",
    );
}

#[test]
fn kata_2268_element_of_the_vec_itself_needs_no_suffix() {
    needs_no_suffix(
        "k2268-plain",
        "    for c in cnt {
        ans += c;
    }",
        "7\n",
    );
}
