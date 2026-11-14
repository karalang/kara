//! Found by kata 1154's Rust arm (B-2026-11-14-44, B-2026-11-14-45).
//!
//! LeetCode's Rust `date.split('-').map(|s| s.parse().unwrap()).collect()`
//! into a `Vec<i32>`, and `days[..(m - 1) as usize].iter().sum::<i32>() + d`
//! returned as an `i32`. After the turbofish and the other Rust spellings
//! were fixed, two errors were left with no edit:
//!
//! - the `s.parse()` in the `map` closure took the adaptor's own `OutV` as
//!   its target, so the `Vec[i32]` the `collect` fills never reached it and
//!   the error said only "name the type";
//! - the sum of a range of `days` narrowing to `i32` named no `days`,
//!   though `days.iter().sum()` gets the suffix fix on its first literal.

use std::process::{Command, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-k1154-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac fix` until nothing applies, then the checked source and the run.
fn fix_and_run(tag: &str, src: &str) -> (String, String, String) {
    let path = write(tag, src);
    for _ in 0..4 {
        let fix = karac(&["fix"], &path);
        if !String::from_utf8_lossy(&fix.stdout).contains("applied") {
            break;
        }
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    (
        fixed,
        String::from_utf8_lossy(&check.stderr).into_owned(),
        String::from_utf8_lossy(&run.stdout).trim().to_string(),
    )
}

#[test]
fn a_parse_in_a_map_closure_takes_the_type_collect_fills() {
    let src = "fn total(date: Str) -> i32 {\n    let v: Vec[i32] = date.split(\"-\").map(|s| s.parse().unwrap()).collect();\n    v[0] + v[1] + v[2]\n}\n\nfn main() {\n    println(f\"{total(\"2019-02-10\")}\");\n}\n";
    let path = write("parse", src);
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        err.contains("here only a later use names it (`i32`)"),
        "{err}"
    );
    let (fixed, err, out) = fix_and_run("parse-fix", src);
    assert!(
        fixed.contains(".map(|s| i32.parse(s).unwrap()).collect();"),
        "{fixed}"
    );
    assert!(!err.contains("error"), "{err}");
    assert_eq!(out, "2031");
}

#[test]
fn a_parse_in_a_map_closure_nothing_types_keeps_its_message() {
    // Only `len()` reads the vector: nothing names the target, so the
    // error stays the plain "name the type" one with no edit.
    let src = "fn main() {\n    let v = \"1-2\".split(\"-\").map(|s| s.parse().unwrap()).collect();\n    println(f\"{v.len()}\");\n}\n";
    let path = write("parse-open", src);
    let check = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    let out = String::from_utf8_lossy(&check.stdout).into_owned();
    assert!(out.contains("name the type"), "{out}");
    assert!(!out.contains("later use names it"), "{out}");
}

#[test]
fn the_sum_of_a_range_of_literals_gets_the_suffix_fix() {
    let src = "fn before(m: i32) -> i32 {\n    let days = [31, 28, 31, 30];\n    days[..(m - 1) as usize].iter().sum() + m\n}\n\nfn main() {\n    println(f\"{before(3)}\");\n}\n";
    let path = write("sum", src);
    let check = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(
        err.contains("the elements of `days` are 'i64'") && err.contains("suffix `i32`"),
        "{err}"
    );
    let (fixed, err, out) = fix_and_run("sum-fix", src);
    assert!(fixed.contains("let days = [31i32, 28, 31, 30];"), "{fixed}");
    assert!(!err.contains("error"), "{err}");
    assert_eq!(out, "62");
}
