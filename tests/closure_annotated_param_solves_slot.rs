//! An annotated closure parameter solves the open parameter its slot
//! names before the body is checked, and the body is checked against the
//! return type that solved. `fold(None, |acc: Option[i64], x| Some(x))`
//! takes `B` from `acc`, so the tail `Some(x)` with `x: ref i64` reads the
//! scalar as its value, as `let o: Option[i64] = Some(x);` does. It was
//! refused with `Option[ref i64]` (B-2026-11-09-36).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-annotated-closure-param-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PROGRAM: &str = r#"fn main() {
    let v = [1, 2, 3].to_vec();
    let a = v.iter().fold(None, |acc: Option[i64], x| Some(x));
    println(a);
    let b = v.iter().fold(None, |acc: Option[i64], x| match acc {
        Some(m) => Some(m.max(x)),
        None => Some(x),
    });
    println(b);
    let c = v.iter().fold(None, |acc: Option[i64], x| {
        let y = x * 2;
        Some(y)
    });
    println(c);
    let d = v.iter().fold(0, |acc: i64, x| acc + x);
    println(d);
    let e = v.iter().fold(None, |acc: Option[i64], x| if x > 1 { Some(x) } else { acc });
    println(e);
}
"#;

const EXPECTED: &str = "Some(3)\nSome(3)\nSome(6)\n6\nSome(3)\n";

/// The default executor (the JIT on an llvm build, the interpreter
/// otherwise) and the interpreter.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

#[test]
fn an_annotated_fold_accumulator_solves_the_slot_before_its_ref_tail() {
    let path = temp("run", PROGRAM);
    for lane in LANES {
        let out = karac(lane, &path);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            EXPECTED,
            "{lane:?}; stderr:\n{stderr}"
        );
        assert!(out.status.success(), "{lane:?}; stderr:\n{stderr}");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_annotated_accumulator_of_another_type_is_still_refused() {
    let path = temp(
        "mismatch",
        "fn main() {
    let v = [1, 2, 3].to_vec();
    let a = v.iter().fold(None, |acc: Option[String], x| Some(x));
    println(a);
}
",
    );
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && stderr.contains("expected 'Option[String]', found 'Option[ref i64]'"),
        "{stderr}"
    );
}
