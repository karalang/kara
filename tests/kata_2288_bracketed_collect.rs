//! Kata 2288's iterator arm was first written with `.collect[Vec[String]]()`,
//! a type in brackets after the method name as Kāra spells a type list.
//! It read as the field `collect` indexed by `Vec[String]`: `karac check`
//! said "no field 'collect'", and `karac fix` applied the deprecated
//! `Vec[..]` literal's fix-it to the index and wrote `.collect[[String]]()`.
//! It is now answered as the turbofish is, with the same fixes: the type
//! moved to the binding, or `T.from_iter(it)` when the result is a
//! receiver; `sum[i64]()` likewise (B-2026-11-18-86).

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

#[test]
fn kata_2288_bracketed_collect_and_sum_are_call_site_type_arguments() {
    let src = "fn main() {
    let s = \"a b c\";
    let words = s.split(\" \").map(|w| w.to_uppercase()).collect[Vec[String]]();
    let joined = s.split(\" \").map(|w| w.to_string()).collect[Vec[String]]().join(\"-\");
    let total = [1, 2, 3].iter().sum[i64]();
    let v = [4, 5];
    let x = v[0];
    println(f\"{words} {joined} {total} {x}\");
}
";
    let file = write("k2288-bracketed-collect", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string()
        + &String::from_utf8_lossy(&check.stdout);
    assert_eq!(
        stderr
            .matches("a type in brackets after a method name is a call-site type argument")
            .count(),
        3,
        "{stderr}"
    );
    assert!(!stderr.contains("no field 'collect'"), "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("let words: Vec[String] = s.split(\" \").map(|w| w.to_uppercase()).collect();")
            && fixed.contains("let joined = Vec[String].from_iter(s.split(\" \").map(|w| w.to_string())).join(\"-\");")
            && fixed.contains("let total: i64 = [1, 2, 3].iter().sum();")
            && !fixed.contains("[["),
        "{fixed}"
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[A, B, C] a-b-c 6 4\n");
}
