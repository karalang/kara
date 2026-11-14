//! Kata 2113's differential compared an arm's answers with
//! `x.iter().map(..).collect()`. `==` takes its own right-hand type
//! (B-2026-10-29-36), so nothing fixes what `collect` builds, and the
//! refusal advised a binding with a type annotation and offered no edit.
//! When the other side's type is known, the collection is now named at
//! the call, `Vec[i64].from_iter(it)`, as the turbofished form's fix
//! already writes it.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn fixes(name: &str, src: &str, want: &str, out: &str) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("a comparison does not fix what `collect` builds"),
        "{stderr}"
    );
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(fixed.contains(want), "{fixed}");
    let ran = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(ran, out);
}

#[test]
fn kata_2113_collect_on_the_right_of_eq_is_named() {
    fixes(
        "k2113-right",
        "fn main() {
    let v = [1, 2, 3].to_vec();
    let w = [2, 4, 6].to_vec();
    println(f\"{w == v.iter().map(|x| x * 2).collect()}\");
}
",
        "w == Vec[i64].from_iter(v.iter().map(|x| x * 2))",
        "true\n",
    );
}

#[test]
fn kata_2113_collect_on_the_left_of_ne_is_named() {
    fixes(
        "k2113-left",
        "fn main() {
    let s = \"abc\";
    let t: String = \"cba\";
    let same = s.chars().rev().collect() != t;
    println(f\"{same}\");
}
",
        "String.from_iter(s.chars().rev()) != t",
        "false\n",
    );
}
