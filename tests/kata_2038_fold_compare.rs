//! Kata 2038's fold arm carries the two pieces before this one as
//! `Option`s that start `None`: `fold((None, None, 0, 0), |(before, last,
//! alice, bob), c| ..)`. Two gaps stood after kata 2036's fix:
//!
//! - `before == Some(c)` compared an `Option[?T]` still open with an
//!   `Option[char]` and was refused ("cannot compare"): two values of one
//!   constructor now unify, as Rust's comparison does;
//! - the tail `(last, Some(c), ..)` handed `last: Option[?B]` to the
//!   accumulator's `Option[?A]` part and was refused: two open types of one
//!   shape are now one.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Whether `src` checks clean, its check output, and what it prints under
/// the MIR interpreter.
fn run(name: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2038-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let (clean, out, err) = karac(&dir, &["check", "cell.kara"]);
    let (_, printed, run_err) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (clean, format!("{out}{err}"), format!("{printed}{run_err}"))
}

#[test]
fn kata_2038_compare_with_an_open_option_solves_it() {
    let src = "fn main() {\n    let (_, n) = \"AAB\".chars().fold((None, 0), |(last, n), c| (Some(c), if last == Some(c) { n + 1 } else { n }));\n    println(n);\n}";
    let (clean, check, out) = run("compare-open", src);
    assert!(clean, "{check}");
    assert_eq!(out, "1\n");
}

#[test]
fn kata_2038_two_open_options_in_a_tail_are_one_type() {
    let src = "fn main() {\n    let (a, b) = \"AAB\".chars().fold((None, None), |(a, b), c| (b, Some(c)));\n    println(f\"{a} {b}\");\n}";
    let (clean, check, out) = run("open-open", src);
    assert!(clean, "{check}");
    assert_eq!(out, "Some(A) Some(B)\n");
}

#[test]
fn kata_2038_fold_carrying_the_two_pieces_before() {
    let src = "fn winner(colors: String) -> bool {\n    let (_, _, alice, bob) = colors.chars().fold((None, None, 0, 0), |(before, last, alice, bob), c| {\n        let triple = before == Some(c) and last == Some(c);\n        let a = if triple and c == 'A' { alice + 1 } else { alice };\n        let b = if triple and c == 'B' { bob + 1 } else { bob };\n        (last, Some(c), a, b)\n    });\n    alice > bob\n}\n\nfn main() {\n    println(winner(\"AAABABB\".to_string()));\n    println(winner(\"ABBBBBBBAAA\".to_string()));\n}";
    let (clean, check, out) = run("two-before", src);
    assert!(clean, "{check}");
    assert_eq!(out, "true\nfalse\n");
}

/// Two different constructors still do not compare.
#[test]
fn kata_2038_compare_of_two_constructors_is_still_refused() {
    let src = "fn main() {\n    let v: Vec[i64] = Vec.new();\n    let o = v.first().cloned();\n    let r: Result[i64, String] = Ok(1);\n    println(o == r);\n}";
    let (clean, check, _) = run("two-heads", src);
    assert!(!clean, "{check}");
    assert!(check.contains("cannot compare"), "{check}");
}
