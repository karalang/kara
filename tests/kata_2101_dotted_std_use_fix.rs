//! Kata 2101's ★ arm began `use std.collections.VecDeque;`, Rust's path
//! written with Kāra's dots. `karac fix` turned the `use` into `import`,
//! and the import then failed with "unknown module `std.collections`":
//! Kāra's collections are in the prelude and no `std.collections` module
//! exists (docs/library/README.md). The `::` spelling already had a fix
//! that deletes the line; the dotted one after `use` now gets the same.
//! An `import` with dots is left to the resolver: syntax.md spells
//! `import std.collections.*`, and `karac fmt` parses it
//! (`tests/fmt_comments.rs`).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

fn fixes_to(name: &str, head: &str) {
    let src = format!(
        "{head}
fn main() {{
    let mut q = VecDeque.new();
    q.push_back(3);
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 2);
    println(f\"{{q.len()}} {{m.len()}}\");
}}
"
    );
    let file = write(name, &src);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        stderr.contains("in its prelude, with no import: delete the line"),
        "{stderr}"
    );
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(fixed.trim_start().starts_with("fn main() {"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "1 1\n");
}

#[test]
fn kata_2101_use_dotted_std_collections_is_deleted() {
    fixes_to("k2101-use-dot", "use std.collections.VecDeque;\n");
}

#[test]
fn kata_2101_use_dotted_std_collections_group_is_deleted() {
    fixes_to(
        "k2101-use-dot-group",
        "use std.collections.{Map, VecDeque};\n",
    );
}
