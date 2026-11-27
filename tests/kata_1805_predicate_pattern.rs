//! B-2026-11-27-70: Rust's `word.split(|c: char| c.is_ascii_alphabetic())`, a
//! char predicate as a string pattern, was refused with a bare "expected
//! 'Str', found 'Fn(char) -> bool'", and a set of chars (`s.split(['a',
//! 'b'])`) with "expected 'Str', found 'Vec[char]'". Both needle kinds are
//! deferred to a `Pattern` trait (deferred.md § String Patterns), so there
//! is no edit to make, but nothing named a way to write it. Kata 1805's Rust
//! solution met it. A closure for `contains`, `starts_with` and `ends_with`
//! has an exact rewrite over the chars, as `find`'s has had since
//! B-2026-11-20-140, and `karac fix` now writes it.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn check_one(tag: &str, body: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-k1805-{tag}-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        format!("fn main() {{\n    let s = \"ab1c2\";\n{body}\n}}\n"),
    )
    .unwrap();
    let out = karac(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success(), "{tag}: check passed");
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn a_char_predicate_as_a_split_pattern_names_the_sequence_split() {
    let text = check_one(
        "split",
        "    let a: Vec[Str] = s.split(|c: char| c.is_ascii_lowercase()).collect();\n    println(f\"{a}\");",
    );
    assert!(text.contains("found 'Fn(char) -> bool'"), "{text}");
    assert!(
        text.contains("a char predicate as a string pattern is deferred"),
        "{text}"
    );
    assert!(text.contains("deferred.md § String Patterns"), "{text}");
    assert!(text.contains("cs.split(|c| ..)"), "{text}");
    assert!(text.contains("1 error(s) found"), "{text}");
}

#[test]
fn a_set_of_chars_as_a_split_pattern_is_named_too() {
    let text = check_one(
        "set",
        "    let a: Vec[Str] = s.split(['a', 'b']).collect();\n    println(f\"{a}\");",
    );
    assert!(
        text.contains("a set of chars as a string pattern is deferred"),
        "{text}"
    );
    assert!(text.contains("1 error(s) found"), "{text}");
}

#[test]
fn a_char_predicate_held_in_a_binding_names_the_chars() {
    let text = check_one(
        "contains",
        "    let p = |c: char| c.is_ascii_digit();\n    println(f\"{s.contains(p)}\");",
    );
    assert!(text.contains("'contains' takes a `Str` pattern"), "{text}");
    assert!(text.contains("s.chars().any(|c| ..)"), "{text}");
    let text = check_one(
        "starts",
        "    let p = |c: char| c.is_ascii_digit();\n    println(f\"{s.starts_with(p)}\");",
    );
    assert!(
        text.contains("s.chars().next().is_some_and(|c| ..)"),
        "{text}"
    );
}

#[test]
fn karac_fix_rewrites_a_closure_for_contains_starts_with_and_ends_with() {
    let path = std::env::temp_dir().join(format!("karac-k1805-fix-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "fn main() {
    let s = \"ab1c2\";
    let a = s.contains(|c: char| c.is_ascii_digit());
    let b = s.starts_with(|c| c == 'a');
    let e = s.ends_with(|c: char| c.is_ascii_lowercase());
    let f = s.find(|c: char| c.is_ascii_digit());
    println(f\"{a} {b} {e} {f}\");
}
",
    )
    .unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        text.contains("a `String`'s `contains` takes a `Str` pattern, not a closure"),
        "{text}"
    );
    assert!(
        text.contains("write `s.chars().any(|c| c.is_ascii_digit())`"),
        "{text}"
    );
    assert!(
        text.contains("write `s.chars().next().is_some_and(|c| c == 'a')`"),
        "{text}"
    );
    assert!(
        text.contains("write `s.chars().last().is_some_and(|c| c.is_ascii_lowercase())`"),
        "{text}"
    );
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        fixed.contains("s.chars().any(|c| c.is_ascii_digit())"),
        "{fixed}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "true true false Some(2)\n",
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn the_routes_the_messages_name_run() {
    let path = std::env::temp_dir().join(format!("karac-k1805-routes-{}.kara", std::process::id()));
    std::fs::write(
        &path,
        "fn main() {
    let s = \"a10b0c7\";
    let cs: Vec[char] = s.chars().collect();
    let mut runs: Vec[String] = Vec.new();
    for run in cs.split(|c| c.is_ascii_lowercase()) {
        let digits: String = run.iter().collect();
        runs.push(digits);
    }
    let any = s.chars().any(|c| c.is_ascii_digit());
    let at = s.char_indices().find(|(_, c)| c.is_ascii_digit());
    let first = s.chars().next().is_some_and(|c| c.is_ascii_digit());
    let last = s.chars().last().is_some_and(|c| c.is_ascii_digit());
    println(f\"{runs} {any} {at} {first} {last}\");
}
",
    )
    .unwrap();
    let run = karac(&["__mir-run", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[, 10, 0, 7] true Some((1, 1)) false true\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
