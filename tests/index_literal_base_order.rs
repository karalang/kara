//! An index expression evaluates its base, then its index (design.md §8.1),
//! when the base is a literal too. An array literal, a `vec!` and a struct
//! literal's field were indexed after the index was evaluated, so
//! `[p(1), p(2)][p(0)]` printed `e0 e1 e2` where it prints `e1 e2 e0`
//! (B-2026-11-14-22). The value selected was right; the order of the
//! side effects (output, a panic, a drop) was not. A call or a named base
//! was already in order.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// Runs `src` on both lanes and returns each lane's stdout.
fn both(tag: &str, src: &str) -> Vec<(String, Output)> {
    let path = std::env::temp_dir().join(format!(
        "karac-index-literal-base-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let outs = [&["run"][..], &["run", "--interp"][..]]
        .into_iter()
        .map(|lane| (format!("{lane:?}"), karac(lane, &path)))
        .collect();
    let _ = std::fs::remove_file(&path);
    outs
}

const P: &str = "fn p(n: i64) -> i64 {\n    println(f\"e{n}\");\n    n\n}\n";

#[test]
fn a_literal_base_is_evaluated_before_its_index() {
    let src = format!(
        "{P}struct W {{\n    v: Vec[i64],\n}}\n\
         fn mk() -> Vec[i64] {{\n    println(\"mk\");\n    vec![1, 2]\n}}\n\
         fn main() {{\n    let a = [p(1), p(2)][p(0)];\n    println(a);\n    \
         let b = vec![p(3), p(4)][p(1)];\n    println(b);\n    \
         let c = W {{ v: vec![p(1), 2] }}.v[p(0)];\n    println(c);\n    \
         let d = (vec![p(2), 3])[p(1)];\n    println(d);\n    \
         let e = mk()[p(0)];\n    println(e);\n    \
         let s = [p(3), p(4)];\n    let f = s[p(0)];\n    println(f);\n}}\n"
    );
    let want = "e1\ne2\ne0\n1\ne3\ne4\ne1\n4\ne1\ne0\n1\ne2\ne1\n3\nmk\ne0\n1\ne3\ne4\ne0\n3\n";
    for (lane, out) in both("order", &src) {
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{lane}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A panicking index: the base's elements are printed before the index's
/// own effect and the bounds panic.
#[test]
fn a_literal_base_runs_before_a_panicking_index() {
    let src = format!("{P}fn main() {{\n    let a = [p(1), p(2)][p(5)];\n    println(a);\n}}\n");
    for (lane, out) in both("panic", &src) {
        assert!(!out.status.success(), "{lane}: ran to the end");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "e1\ne2\ne5\n",
            "{lane}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
