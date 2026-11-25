//! B-2026-11-24-39: `karac fix`'s `#[derive(PartialEq)]` line for a `pub
//! struct` or `pub enum` at the top level was indented by the width of
//! `pub `: the line took the `struct` keyword's column as the item's
//! indent. It now takes the column of the item's first modifier, so the
//! derive lines up with the declaration. Found beside kata 1325.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn kata_1325_derive_line_matches_a_pub_declaration() {
    let src = "pub struct P {
    v: i32,
}

pub enum Op {
    And,
    Or,
}

struct Q {
    w: i32,
}

fn main() {
    let a = P { v: 1 };
    let b = P { v: 1 };
    let c = Q { w: 2 };
    let d = Q { w: 3 };
    println(f\"{a == b} {Op.And == Op.Or} {c == d}\");
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-kata-1325-derive-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let file = path.to_str().unwrap();
    karac(&["fix", file]);
    let text = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert!(
        text.starts_with("#[derive(PartialEq)]\npub struct P {"),
        "{text}"
    );
    assert!(
        text.contains("\n#[derive(PartialEq)]\npub enum Op {"),
        "{text}"
    );
    assert!(
        text.contains("\n#[derive(PartialEq)]\nstruct Q {"),
        "{text}"
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "true false false\n");
}
