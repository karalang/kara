//! An unannotated closure parameter stays open until its first constraining
//! use, and takes the literal default only at the end of the body, as an
//! integer literal does (design.md §8 Type variables; review-thread ruling
//! 2026-11-24, B-2026-11-24-15). Before, `|x| x + 1` settled `x` to `i64` at
//! the closure, and a literal argument `f(2)` settled at the call, so
//! `let y: i32 = f(2)` was refused as a narrowing.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], src: &str, tag: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-closure-open-literal-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn runs_to(tag: &str, src: &str, want: &str) {
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, src, tag);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn closure_param_open_literal_fixed_by_later_uses() {
    runs_to(
        "uses",
        "fn main() {\n    let f = |x| x + 1;\n    let a: i32 = 2;\n    let y: i32 = f(a);\n    \
         let g = |x| x;\n    let z: i32 = g(2);\n    let h = |x| x * 2;\n    let w = h(4);\n    \
         let v: u8 = w;\n    let k = |x| { let one = 1; x + one };\n    let b: u8 = 250;\n    \
         let c: u8 = k(b);\n    println(f\"{y} {z} {v} {c}\");\n}\n",
        "3 2 8 251\n",
    );
}

#[test]
fn closure_param_open_literal_defaults_at_the_end() {
    runs_to(
        "default",
        "fn main() {\n    let f = |x| x + 1;\n    println(f(3000000000));\n    let g = |x| x * 1.5;\n    \
         println(g(2.0));\n}\n",
        "3000000001\n3.0\n",
    );
}

#[test]
fn closure_param_open_literal_range_checked_at_the_fixed_type() {
    let out = karac(
        &["check"],
        "fn main() {\n    let g = |x| x;\n    let y: i32 = g(3000000000);\n    println(y);\n}\n",
        "range",
    );
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("out of range for 'i32'"), "{err}");
}
