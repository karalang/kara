//! B-2026-11-19-110: a `par for` at the end of a block of a function
//! that returns a value is the block's value, as a `for` in a value
//! position is (core semantics §11.4), not a statement whose collected
//! value is thrown away (E0213 "this block has no value").

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-par-for-tail-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn runs(tag: &str, src: &str, want: &str) {
    for sub in ["check", "__mir-run"] {
        let out = karac(tag, sub, src);
        assert!(
            out.status.success(),
            "{tag} {sub}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if sub == "__mir-run" {
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{tag}");
        }
    }
}

#[test]
fn par_for_at_a_functions_tail_is_its_value() {
    runs(
        "tail",
        "fn doubled(xs: Vec[i64]) -> Vec[i64] {
    par for x in xs.iter() { x * 2 }
}
fn main() { println(f\"{doubled([1, 2, 3])}\"); }
",
        "[2, 4, 6]\n",
    );
}

#[test]
fn par_for_with_a_semicolon_before_a_tail_stays_a_statement() {
    runs(
        "statement",
        "fn count(xs: Vec[i64]) -> i64 {
    let ys: Vec[i64] = par for x in xs.iter() { x + 1 };
    par for y in ys.iter() { y * 2 };
    ys.len()
}
fn main() { println(count([5])); }
",
        "1\n",
    );
}
