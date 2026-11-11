//! Kata 1122's solution arm, written as Rust, begins with
//! `use std::collections::HashMap;` between two blank lines. `karac fix`
//! deletes the line and its newline, which left the blank lines either
//! side of it as two in a row. It now takes one of them too, and at the
//! top of a file the blank line after it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kata_1122_use_line_blank_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The text one `karac fix` leaves.
fn fixed(name: &str, src: &str) -> String {
    let dir = scratch(name);
    let file = dir.join("m.kara");
    std::fs::write(&file, src).unwrap();
    karac(&dir, &["fix", "m.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

const MAIN: &str = "fn main() {
    let mut m: Map[i32, i64] = Map.new();
    m[1] = 2;
    println(f\"{m.len()}\");
}
";

#[test]
fn a_rust_use_between_blank_lines_leaves_one_blank_line() {
    let src = format!("// A comment.\n\nuse std::collections::HashMap;\n\n{MAIN}");
    assert_eq!(fixed("between", &src), format!("// A comment.\n\n{MAIN}"));
}

#[test]
fn a_rust_use_at_the_top_of_a_file_takes_the_blank_line_after_it() {
    let src = format!("use std::collections::HashMap;\n\n{MAIN}");
    assert_eq!(fixed("top", &src), MAIN);
}

#[test]
fn a_rust_use_with_code_right_after_it_leaves_the_code_where_it_was() {
    let src = format!("// A comment.\n\nuse std::collections::HashMap;\n{MAIN}");
    assert_eq!(fixed("after", &src), format!("// A comment.\n\n{MAIN}"));
}
