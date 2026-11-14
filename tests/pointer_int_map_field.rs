//! B-2026-11-14-24: a struct field written `Map[usize, _]` (Rust's
//! `HashMap<usize, i32>` as `karac fix` carries it) and filled from
//! `enumerate()`'s `i64` index got a bare "expected 'Map[usize, i64]',
//! found 'Map[i64, i64]'" and no fix: a `Map.new()` carries its default
//! hasher as a third type argument the written field does not, so the two
//! never read as one shape and the written `usize` was never offered `i64`,
//! as it is for a `Vec[usize]` field.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn check_then_fix(tag: &str, field: &str, insert: &str) -> (String, String, String) {
    let src = format!(
        "struct S {{\n    seen: {field},\n}}\n\n\
fn build(nums: Vec[i64]) -> S {{\n    let mut seen = Map.new();\n    for (i, x) in nums.iter().enumerate() {{\n        if x != 0 {{\n            seen.insert({insert});\n        }}\n    }}\n    S {{ seen }}\n}}\n\n\
fn main() {{\n    println(build([1, 0, 2]).seen.len());\n}}\n"
    );
    let dir = std::env::temp_dir().join(format!("karac-mapusize-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let before = String::from_utf8_lossy(&karac(&["check", p]).stderr).into_owned();
    let fixed = karac(&["fix", p]);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let run = karac(&["run", "--interp", p]);
    assert!(
        run.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (
        before,
        after,
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

#[test]
fn a_map_usize_key_field_gets_the_i64_fix() {
    let (before, after, out) = check_then_fix("key", "Map[usize, i64]", "i, x");
    assert!(before.contains("so write `i64` here"), "{before}");
    assert!(after.contains("seen: Map[i64, i64],"), "{after}");
    assert_eq!(out, "2\n");
}

#[test]
fn a_map_usize_value_field_gets_the_i64_fix() {
    let (before, after, out) = check_then_fix("value", "Map[i64, usize]", "x, i");
    assert!(before.contains("so write `i64` here"), "{before}");
    assert!(after.contains("seen: Map[i64, i64],"), "{after}");
    assert_eq!(out, "2\n");
}
