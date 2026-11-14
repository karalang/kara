//! A range view made in a `match` / `if let` scrutinee whose value holds
//! no borrow (`v[1..].iter().position(..)` is an `Option[i64]`) leaves
//! nothing borrowed in the arms, as the same value bound by `let` first
//! does; a scrutinee whose value still points into the view keeps it
//! borrowed. Kata 1536's `match zeros[i..].iter().position(..)` with a
//! `zeros.remove` in the arm was refused. B-2026-11-14-5.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-scrutinee-view-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "m.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let run = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "m.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    if out.status.success() {
        (true, String::from_utf8_lossy(&run.stdout).into_owned())
    } else {
        (false, String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[test]
fn match_on_a_position_in_a_range_view_frees_the_vec() {
    let src = "fn main() {\n    let mut v: Vec[i64] = [3, 1, 4, 1, 5];\n    match v[1..].iter().position(|x| *x == 4) {\n        None => println(\"none\"),\n        Some(at) => {\n            v.remove(at + 1);\n        }\n    }\n    println(f\"{v}\");\n}\n";
    let (ok, out) = check("match", src);
    assert!(ok, "{out}");
    assert_eq!(out, "[3, 1, 1, 5]\n");
}

#[test]
fn if_let_on_a_position_in_a_range_view_frees_the_vec() {
    let src = "fn main() {\n    let mut v: Vec[i64] = [3, 1, 4, 1, 5];\n    if let Some(at) = v[1..].iter().position(|x| *x == 4) {\n        v.insert(0, at);\n    }\n    println(f\"{v}\");\n}\n";
    let (ok, out) = check("iflet", src);
    assert!(ok, "{out}");
    assert_eq!(out, "[1, 3, 1, 4, 1, 5]\n");
}

#[test]
fn match_on_a_reference_into_a_range_view_keeps_the_borrow() {
    let src = "fn main() {\n    let mut v: Vec[i64] = [3, 1, 4, 1, 5];\n    match v[1..].first() {\n        None => println(\"none\"),\n        Some(x) => {\n            v.push(9);\n            println(f\"{x}\");\n        }\n    }\n}\n";
    let (ok, err) = check("keeps", src);
    assert!(!ok, "a `ref` into the view must keep `v` borrowed");
    assert!(err.contains("cannot be borrowed as `mut ref T`"), "{err}");
}
