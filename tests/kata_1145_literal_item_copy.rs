//! Found writing kata 1145's differential (B-2026-11-12-18).
//!
//! An adaptor over a range lends each item to its predicate, and over
//! `Copy` values the parameter is a copy (core-semantics.md §5.10,
//! B-2026-10-13-32), so `(1..n).filter(|v| *v ..)` is refused with "drop
//! the `*`". Over a range of bare literals the item is a literal no use has
//! typed yet, which was not taken for a copy: the parameter kept the
//! library's `ref` and `(1..10).filter(|v| *v ..)` was accepted. Now a
//! literal item is a copy as an `i64`'s is, and both ranges read alike.

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
        "karac-kata-1145-literal-item-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

/// `karac check`'s stderr, and what the program prints after `karac fix`.
fn check_then_fix_run(tag: &str, src: &str) -> (String, String) {
    let path = temp(tag, src);
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = karac(&["fix"], &path);
    let run = karac(&["__mir-run"], &path);
    let out =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    let _ = std::fs::remove_file(&path);
    (err, out)
}

#[test]
fn a_literal_range_item_is_lent_to_its_predicate_as_a_copy() {
    let shapes = [
        (
            "filter",
            "fn main() {\n    let a: Vec[i64] = (1..10).filter(|x| *x % 2 == 0).collect();\n    println(f\"{a}\");\n}\n",
            "[2, 4, 6, 8]\n",
        ),
        (
            "position",
            "fn main() {\n    let p = (0..5).position(|x| *x == 3);\n    println(f\"{p}\");\n}\n",
            "Some(3)\n",
        ),
        (
            "variable",
            "fn main() {\n    let n = 10;\n    let c = (1..n).filter(|x| *x % 3 == 0).count();\n    println(f\"{c}\");\n}\n",
            "3\n",
        ),
    ];
    for (tag, src, want) in shapes {
        let (err, out) = check_then_fix_run(tag, src);
        assert!(err.contains("so drop the `*`"), "{tag}: {err}");
        assert_eq!(out, want, "{tag}");
    }
}

#[test]
fn a_literal_range_item_read_bare_is_still_typed_by_its_use() {
    let (err, out) = check_then_fix_run(
        "typed",
        "fn main() {\n    let a: Vec[u8] = (1..10).filter(|x| x % 3 == 0).collect();\n    let b = (1..10).filter(|x| x > 6).count();\n    println(f\"{a} {b}\");\n}\n",
    );
    assert!(!err.contains("error"), "{err}");
    assert_eq!(out, "[3, 6, 9] 3\n");
}
