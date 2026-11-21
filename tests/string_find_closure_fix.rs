//! Found writing kata 2734 (Lexicographically Smallest String After
//! Substring Operation).
//!
//! B-2026-11-20-140: `s.find(|c| c != 'a')`, Rust's `str::find` with a closure, was
//! refused with "'find' expects a String or char needle", a message that
//! predates `find` taking a `Str` pattern, and with no fix. The first char
//! that passes a test is `char_indices().find(..)`, its byte offset the
//! `.map(|(i, _)| i)` of that; `karac fix` now writes it.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(cmd: &str, src: &str) -> (String, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-find-closure-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        after,
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const WITH_CLOSURE: &str = "fn first(s: String) -> i64 {
    s.find(|c| c != 'a').unwrap_or(-1)
}

fn main() {
    let s = \"aab\".to_string();
    let i = s.find(|ch| ch == 'b');
    println(f\"{i} {first(s)}\");
}
";

const FIXED: &str = "fn first(s: String) -> i64 {
    s.char_indices().find(|(_, c)| c != 'a').map(|(i, _)| i).unwrap_or(-1)
}

fn main() {
    let s = \"aab\".to_string();
    let i = s.char_indices().find(|(_, ch)| ch == 'b').map(|(i, _)| i);
    println(f\"{i} {first(s)}\");
}
";

#[test]
fn string_find_closure_names_char_indices() {
    let (out, _) = karac("check", WITH_CLOSURE);
    assert!(
        out.contains("a `String`'s `find` takes a `Str` pattern, not a closure"),
        "{out}"
    );
    assert!(
        out.contains("write `s.char_indices().find(|(_, c)| c != 'a').map(|(i, _)| i)`"),
        "{out}"
    );
}

#[test]
fn string_find_closure_fix_writes_char_indices() {
    let (_, after) = karac("fix", WITH_CLOSURE);
    assert_eq!(after, FIXED);
    let (out, _) = karac("check", FIXED);
    assert!(out.contains("All checks passed"), "{out}");
}

#[test]
fn string_find_closure_fixed_runs() {
    assert_eq!(run(FIXED), "Some(2) 2\n");
}

#[test]
fn string_find_str_pattern_unchanged() {
    let src = "fn main() {
    let s = \"aab\".to_string();
    let j = s.find(\"b\");
    println(f\"{j}\");
}
";
    assert_eq!(run(src), "Some(2)\n");
}
