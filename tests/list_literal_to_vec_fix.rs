//! Found writing kata 2786 (Visit Array Positions to Maximize Score).
//!
//! B-2026-11-20-148: `let mut rows: Vec[Row] = [first].to_vec();`, Rust's
//! habit of making a `Vec` from an array, with `Row` a struct that has no
//! `Clone`. A list literal is already the `Vec` (design.md § Collection
//! literals), so `.to_vec()` only copies a temporary; the refusal said
//! to add `#[derive(Copy, Clone)]` to `Row`, and `karac fix` added it. The edit
//! is to drop the copy, which the fix now does; a cloneable element keeps
//! `.to_vec()` as written.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

fn karac(cmd: &str, src: &str) -> (String, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "karac-list-literal-to-vec-{}-{n}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([cmd, path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        after,
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

const WRITTEN: &str = "struct Row {
    even: Option[i64],
}

fn main() {
    let first = Row { even: Some(2) };
    let mut rows: Vec[Row] = [first].to_vec();
    rows.push(Row { even: None });
    let mut more = [Row { even: Some(4) }].to_vec();
    more.push(Row { even: Some(6) });
    println(f\"{rows.len()} {more.len()} {rows[0].even ?? 0} {more[1].even ?? 0}\");
}
";

const FIXED: &str = "struct Row {
    even: Option[i64],
}

fn main() {
    let first = Row { even: Some(2) };
    let mut rows: Vec[Row] = [first];
    rows.push(Row { even: None });
    let mut more = [Row { even: Some(4) }];
    more.push(Row { even: Some(6) });
    println(f\"{rows.len()} {more.len()} {rows[0].even ?? 0} {more[1].even ?? 0}\");
}
";

#[test]
fn list_literal_to_vec_says_drop_it() {
    let (out, _) = karac("check", WRITTEN);
    assert!(
        out.contains(":7:30: `.to_vec()` copies a list literal that is already a 'Vec[Row]', and its elements have no `.clone()`: drop `.to_vec()`, the literal is the list"),
        "{out}"
    );
    assert!(!out.contains("derive"), "{out}");
    assert!(out.contains("2 error(s) found"), "{out}");
}

#[test]
fn list_literal_to_vec_fix_drops_the_copy() {
    let (_, after) = karac("fix", WRITTEN);
    assert_eq!(after, FIXED);
    let (out, _) = karac("check", FIXED);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(FIXED), "2 2 2 6\n");
}

#[test]
fn cloneable_list_literal_to_vec_unchanged() {
    let src = "fn main() {
    let w: Vec[i64] = [1, 2].to_vec();
    let s: Vec[String] = [\"a\".to_string()].to_vec();
    println(f\"{w.len()} {s.len()}\");
}
";
    let (out, _) = karac("check", src);
    assert!(out.contains("All checks passed"), "{out}");
    assert_eq!(run(src), "2 1\n");
}

#[test]
fn bound_vec_to_vec_still_names_the_derive() {
    // A `Vec` held in a binding is copied for real: the element needs
    // `Clone`, and the derive stays the edit.
    let src = "struct Row {
    a: i64,
}

fn main() {
    let rows: Vec[Row] = [Row { a: 1 }];
    let copy = rows.to_vec();
    println(f\"{copy.len()}\");
}
";
    let (out, _) = karac("check", src);
    assert!(out.contains("add `#[derive(Copy, Clone)]` to it"), "{out}");
}
