//! Kata 2049's Rust arm: LeetCode's template passes its children lists as
//! `children: &Vec<Vec<usize>>`, which `karac fix` spells
//! `ref Vec[Vec[usize]]`, while the lists it builds hold the `i64` of
//! `0..parents.len()`. A scalar `usize` meeting an `i64` already got its fix
//! to `i64` (B-2026-11-06-3); a `usize` nested in a parameter's type was
//! refused with "a number held inside a collection ... keeps its type" and no
//! fix. The nested meeting now counts too, so every written `usize` is fixed.

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
fn kata_2049_usize_nested_in_a_parameter_type_fixes_to_i64() {
    let src = "fn visit(v: usize, children: ref Vec[Vec[usize]]) -> i64 {
    let mut size = 1;
    for c in children[v].iter() {
        size += visit(c, children);
    }
    size
}

fn main() {
    let parents = [-1, 0, 0, 1];
    let n = parents.len();
    let mut children = Vec.filled(n, Vec.new());
    for i in 1..n {
        children[parents[i]].push(i);
    }
    println(visit(0, children));
}
";
    let file = write("k2049-nested", src);
    let check = karac(&["check", &file]);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(msg.contains("so write `i64` here"), "{msg}");
    let _ = karac(&["fix", &file]);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(!fixed.contains("usize"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "4\n");
}

#[test]
fn kata_2049_nested_mismatch_of_other_widths_keeps_no_fix() {
    let src = "fn total(xs: ref Vec[i32]) -> i32 {
    xs.iter().sum()
}

fn main() {
    let n = [7, 8, 9].len();
    let mut xs = Vec.new();
    for i in 0..n {
        xs.push(i);
    }
    println(total(xs));
}
";
    let file = write("k2049-i32", src);
    let check = karac(&["check", &file]);
    let dry = karac(&["fix", "--dry-run", &file]);
    let _ = std::fs::remove_file(&file);
    let msg = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(msg.contains("keeps its type"), "{msg}");
    let plan = String::from_utf8_lossy(&dry.stdout).into_owned();
    assert!(!plan.contains("→ `i64`"), "{plan}");
}
