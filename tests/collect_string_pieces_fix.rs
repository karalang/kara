//! Found writing kata 1363's sorted arm (B-2026-11-21-63).
//!
//! Rust writes a number from its digits as
//! `digits.iter().map(|d| d.to_string()).collect()` into a `String`, through
//! `FromIterator<String> for String`. A Kāra `String` gathers `char`s, so
//! the refusal says to collect a `Vec[String]` and `join("")`
//! (B-2026-11-10-34), but `karac fix` had nothing to apply. The rewrite is
//! local and keeps the type, `Vec[String].from_iter(..).join("")` being a
//! `String` as the `collect` was, so it is now the fix for `String`
//! pieces; `Str` pieces keep the help alone.

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
        "karac-collect-join-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn fixed_and_run(tag: &str, src: &str) -> (String, String) {
    let path = temp(tag, src);
    let before = karac(&["check"], &path);
    let said = String::from_utf8_lossy(&before.stderr).into_owned();
    assert!(!before.status.success(), "{said}");
    assert!(
        said.contains("collect the pieces into a `Vec[String]` and join them"),
        "{said}"
    );
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        run.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    (fixed, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn string_pieces_collected_as_a_return_value_are_joined_by_the_fix() {
    let (fixed, out) = fixed_and_run(
        "tail",
        "fn number(digits: Vec[i64]) -> String {
    digits.iter().map(|d| d.to_string()).collect()
}

fn main() {
    println(number([9, 8, 1]));
}
",
    );
    assert!(
        fixed.contains("Vec[String].from_iter(digits.iter().map(|d| d.to_string())).join(\"\")"),
        "{fixed}"
    );
    assert_eq!(out, "981\n");
}

#[test]
fn string_pieces_collected_into_an_annotated_binding_are_joined_by_the_fix() {
    let (fixed, out) = fixed_and_run(
        "binding",
        "fn main() {
    let v = [\"a\", \"b\"].to_vec();
    let s: String = v.iter().map(|x| f\"[{x}]\").collect();
    println(s);
}
",
    );
    assert!(
        fixed.contains("let s: String = Vec[String].from_iter("),
        "{fixed}"
    );
    assert_eq!(out, "[a][b]\n");
}

#[test]
fn str_pieces_keep_the_help_and_get_no_join_fix() {
    let path = temp(
        "str",
        "fn main() {
    let s: String = \"a b\".split(\" \").collect();
    println(s);
}
",
    );
    let before = karac(&["check"], &path);
    let said = String::from_utf8_lossy(&before.stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(!before.status.success(), "{said}");
    assert!(!fixed.contains("Vec[String].from_iter("), "{fixed}");
}
