//! design.md §8 Literals: a number literal that nothing types where it is
//! written stays open for a later use to fix, inside a tuple too.
//! - B-2026-11-19-41: a `let`'s tuple that mixes a literal with a typed part
//!   (`(0, w[0])`) held the literal at `i64`; a comparison of two tuples that
//!   both hold open literals now fixes each side's from the other's.
//! - B-2026-11-18-72: a list of tuples that mix numbers with strings
//!   (`[("x", 2)]`) held the number at `i64`, so a closure that destructures
//!   an item could not `collect` into `Vec[(String, i32)]`, with `v` or with
//!   Rust's `*v`.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-mixed-tuple-literal-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn runs_as(tag: &str, body: &str, want: &str) {
    let src = format!("fn main() {{\n{body}\n}}\n");
    let check = karac(tag, &["check"], &src);
    assert!(check.status.success(), "{tag}: check: {}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(tag, &[b], &src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            text(&out)
        );
        assert!(out.status.success(), "{tag} on {b}: {:?}", out.status);
    }
}

#[test]
fn mixed_tuple_literal_is_fixed_by_a_later_comparison_and_assignment() {
    runs_as(
        "mixed-cmp",
        "let v: Vec[i32] = [3, 5]; let w: Vec[u8] = [3, 5];\n\
         let mut best = (0, w[0]); let p = (v[0], w[1]);\n\
         if p > best { best = p; }\n\
         println(f\"{best.0} {best.1}\");",
        "3 5\n",
    );
    // Both sides hold an open literal: each fixes the other's.
    runs_as(
        "both-open",
        "let v: Vec[i32] = [3, 5];\n\
         let mut best = (0, 1); let p = (v[0], 2);\n\
         if p > best { best = p; }\n\
         println(f\"{best.0} {best.1}\");",
        "3 2\n",
    );
    runs_as(
        "nested",
        "let w: Vec[u8] = [3, 5];\n\
         let mut m = (-1, w[0], (2, w[1]));\n\
         let n: (i16, u8, (i8, u8)) = (4, 1, (1, 1));\n\
         if n > m { m = n; }\n\
         println(f\"{m.0} {m.2.0}\");",
        "4 1\n",
    );
    runs_as(
        "floats",
        "let w: Vec[f32] = [1.5, 2.5];\n\
         let mut acc = (0.0, w[0], 1);\n\
         let x: (f32, f32, i32) = (w[1], w[1], 2);\n\
         if x > acc { acc = x; }\n\
         println(f\"{acc.0} {acc.2}\");",
        "2.5 2\n",
    );
}

#[test]
fn mixed_tuple_literal_takes_an_annotation_and_its_range_check() {
    runs_as(
        "annotated",
        "let w: Vec[u8] = [3, 5]; let a = (7, w[0]); let c: (i8, u8) = a; println(c.0);",
        "7\n",
    );
    // Nothing fixes it: the default, as before.
    runs_as(
        "default",
        "let w: Vec[u8] = [3, 5]; let t = (0, w[0]); println(t.0 + 9000000000);",
        "9000000000\n",
    );
    let out = karac(
        "range",
        &["check"],
        "fn main() { let w: Vec[u8] = [3, 5]; let a = (300, w[0]); let c: (u8, u8) = a; println(c.0); }\n",
    );
    assert!(
        !out.status.success() && text(&out).contains("integer literal 300 out of range for 'u8'"),
        "{}",
        text(&out)
    );
}

#[test]
fn list_of_mixed_tuples_collects_at_the_annotated_number_type() {
    runs_as(
        "collect",
        "let r = [(\"x\", 2), (\"yy\", 300)];\n\
         let out: Vec[(String, i32)] = r.iter().map(|(n, v)| (n.to_string(), v)).collect();\n\
         println(f\"{out[1].0} {out[1].1}\");",
        "yy 300\n",
    );
    runs_as(
        "collect-deref",
        "let r = [(\"x\", 2)];\n\
         let out: Vec[(String, i32)] = r.iter().map(|(n, v)| (n.to_string(), *v)).collect();\n\
         println(out[0].1);",
        "2\n",
    );
    runs_as(
        "collect-numbers-deref",
        "let t = [(1, 2)];\n\
         let out: Vec[(i32, i32)] = t.iter().map(|(a, b)| (*a, *b)).collect();\n\
         println(out[0].0 + out[0].1);",
        "3\n",
    );
    runs_as(
        "list-default",
        "let r = [(\"x\", 2)]; let n = r[0].1; println(n + 9000000000);",
        "9000000002\n",
    );
}
