//! Kata 2085's Rust arm: LeetCode's template counts the words of one list
//! seen once in the other with `c2.get(*w) == Some(&1)`. `karac fix` made the
//! `&` a `ref`, and `Some(ref 1)` is refused (E_REF_OPERAND_UNSUPPORTED): a
//! literal has no place to borrow. The edit now drops the `&` before a
//! scalar literal, and Kāra compares the value as it is.

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
fn kata_2085_ref_of_literal_fix_drops_the_amp() {
    let src = "fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert(\"a\", 1);
    m.insert(\"b\", 2);
    let once = m.get(\"a\") == Some(&1);
    let twice = m.get(\"b\") == Some(&1);
    let c: Option[char] = Some('x');
    let x = c == Some(&'x');
    let f: Option[f64] = Some(0.5);
    let h = f == Some(&0.5);
    let t: Option[bool] = Some(true);
    let y = t == Some(&true);
    println(f\"{once} {twice} {x} {h} {y}\");
}
";
    let file = write("k2085-ref-lit", src);
    let _ = karac(&["fix", &file]);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("== Some(1);"), "{fixed}");
    assert!(fixed.contains("Some('x')"), "{fixed}");
    assert!(!fixed.contains("ref"), "{fixed}");
    assert!(
        check.status.success(),
        "{}\n{fixed}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "true false true true true\n");
}

/// `&v` of a binding passed as an argument keeps its own edit: the `&`
/// dropped, as a parameter borrows already.
#[test]
fn kata_2085_ref_of_binding_still_becomes_ref() {
    let src = "fn first(v: ref Vec[i64]) -> i64 { v[0] }

fn main() {
    let v = [4, 5];
    println(f\"{first(&v)}\");
}
";
    let file = write("k2085-ref-bind", src);
    let _ = karac(&["fix", &file]);
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(fixed.contains("first(v)"), "{fixed}");
}
