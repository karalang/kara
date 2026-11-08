//! B-2026-11-03-28, found porting the mini_mvcc app: `x.as_str()` passed
//! where a `ref String` is wanted, with `x` a `String`, got the fix that
//! appends `.to_string()`, so `karac fix` wrote
//! `levels.contains(w[2].as_str().to_string())`: a fresh copy of text the
//! place already holds. Dropping the `.as_str()` borrows `x` itself. Where an
//! owned `String` is wanted, dropping it would move `x`, so that fix stays.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn fix_until_clean(name: &str, src: &str) -> (String, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    let file = path.to_str().unwrap().to_string();
    std::fs::write(&path, src).unwrap();
    for _ in 0..3 {
        let _ = karac(&["fix", &file]);
    }
    let fixed = std::fs::read_to_string(&path).unwrap();
    let check = karac(&["check", &file]);
    assert!(
        check.status.success(),
        "{name}: {}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&path);
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    (fixed, out)
}

#[test]
fn as_str_where_a_borrowed_string_is_wanted_is_dropped() {
    let src = "fn named(names: Vec[String], s: String) -> bool {
    names.contains(s)
}

fn main() {
    let w: Vec[String] = [\"si\".to_string(), \"x\".to_string()].to_vec();
    if [\"rc\", \"si\"].contains(w[0].as_str()) {
        println(\"level\");
    }
    let name = w[1].clone();
    if named([\"x\".to_string()].to_vec(), name.as_str()) {
        println(f\"named {name}\");
    }
}
";
    let path = std::env::temp_dir().join(format!("karac-as-str-msg-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", path.to_str().unwrap()]);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = std::fs::remove_file(&path);
    assert!(
        err.contains("drop the `.as_str()`, and the `String` itself is borrowed"),
        "{err}"
    );
    let (fixed, out) = fix_until_clean("as-str-borrow", src);
    // A sequence's `contains` takes the element's view (B-2026-11-08-55),
    // so the `.as_str()` there is no longer an error and stays.
    assert!(fixed.contains("].contains(w[0].as_str()) {"), "{fixed}");
    assert!(fixed.contains("].to_vec(), name) {"), "{fixed}");
    assert!(!fixed.contains(".to_string())"), "{fixed}");
    assert_eq!(out, "level\nnamed x\n");
}

#[test]
fn as_str_where_an_owned_string_is_wanted_still_copies() {
    let src = "fn keep(s: own String) -> i64 {
    s.len()
}

fn main() {
    let w: Vec[String] = [\"si\".to_string()].to_vec();
    println(keep(w[0].as_str()));
    let x: String = w[0].as_str();
    println(f\"{x} {w[0]}\");
}
";
    let (fixed, out) = fix_until_clean("as-str-owned", src);
    assert!(fixed.contains("keep(w[0].as_str().to_string())"), "{fixed}");
    assert!(
        fixed.contains("let x: String = w[0].as_str().to_string();"),
        "{fixed}"
    );
    assert_eq!(out, "2\nsi si\n");
}
