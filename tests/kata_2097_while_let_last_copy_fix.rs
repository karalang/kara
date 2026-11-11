//! Kata 2097's renamed-ids arm walks Hierholzer's stack with
//! `while let Some(top) = stack.last() { .. stack.push(..) .. }`. `last`
//! returns `Option[ref T]`, so `top` borrows `stack` for the whole body and
//! the push is refused (E0516), which is right. The repair the message
//! names, a copy of the value, was machine-applied only to a `let`
//! initializer (B-2026-10-22-5); a `while let` or `if let` scrutinee got
//! none. It now gets `.cloned()` when the element is `Copy` or `shared`.

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
fn kata_2097_while_let_last_gets_cloned_fix() {
    let src = "fn main() {
    let outs = [[1, 2].to_vec(), [2].to_vec(), Vec.new()].to_vec();
    let mut used = [0, 0, 0].to_vec();
    let mut stack = [0].to_vec();
    let mut path: Vec[i64] = Vec.new();
    while let Some(top) = stack.last() {
        if used[top] < outs[top].len() {
            stack.push(outs[top][used[top]]);
            used[top] += 1;
        } else {
            path.push(top);
            stack.pop();
        }
    }
    let mut words = [\"a\".to_string()].to_vec();
    if let Some(w) = words.first() {
        words.push(w.clone());
    }
    println(f\"{path} {words.len()}\");
}
";
    let file = write("k2097-while-let-last", src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(stderr.contains("[E0516]"), "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(
        fixed.contains("while let Some(top) = stack.last().cloned() {"),
        "{fixed}"
    );
    // A `String` element would be a deep copy: no fix-it, the refusal stays.
    assert!(
        fixed.contains("if let Some(w) = words.first() {"),
        "{fixed}"
    );
}

#[test]
fn kata_2097_while_let_last_fixed_runs() {
    let src = "fn main() {
    let outs = [[1, 2].to_vec(), [2].to_vec(), Vec.new()].to_vec();
    let mut used = [0, 0, 0].to_vec();
    let mut stack = [0].to_vec();
    let mut path: Vec[i64] = Vec.new();
    while let Some(top) = stack.last() {
        if used[top] < outs[top].len() {
            stack.push(outs[top][used[top]]);
            used[top] += 1;
        } else {
            path.push(top);
            stack.pop();
        }
    }
    println(f\"{path}\");
}
";
    let file = write("k2097-while-let-run", src);
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "[2, 1, 2, 0]\n");
}
