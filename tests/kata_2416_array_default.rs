//! Kata 2416's Rust template is LeetCode's trie, `#[derive(Default)] struct
//! Trie { children: [Option<Box<Trie>>; 26], cnt: i32 }`. `karac fix`
//! writes the field `Array[Option[Trie], 26]`, and the derive was then
//! refused: `Array[T, N]` had no `Default`, where Rust's `[T; N]` has one
//! whenever `T` does. A derived `default()` now fills an array field with
//! `N` defaults of its element (B-2026-11-21-16).

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

#[test]
fn kata_2416_derived_default_fills_an_array_field() {
    let src = "#[derive(Default)]
struct Trie {
    children: Array[Option[Trie], 26],
    cnt: i32,
}

#[derive(Default)]
struct Grid {
    cells: Array[i64, 4],
    names: Array[String, 2],
    pairs: Array[(i64, bool), 3],
}

fn main() {
    let mut root = Trie.default();
    root.children[3] = Some(Trie.default());
    let mut node = mut ref root;
    node = node.children[3].get_or_insert_with(|| Trie.default());
    node.cnt += 5;
    let none = root.children.iter().filter(|c| c.is_none()).count();
    let g = Grid.default();
    println(f\"{none} {root.children[3].as_ref().unwrap().cnt} {g.cells[3]} {g.names[1].len()} {g.pairs[2].0} {g.pairs[2].1}\");
}
";
    let file = write("k2416-array-default", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "25 5 0 0 0 false\n");
}

#[test]
fn kata_2416_array_of_an_element_without_default_is_still_refused() {
    let src = "struct Plain {
    x: i64,
}

#[derive(Default)]
struct Holder {
    items: Array[Plain, 2],
}

fn main() {
    let h = Holder.default();
    println(f\"{h.items[0].x}\");
}
";
    let file = write("k2416-array-no-default", src);
    let check = karac(&["check", &file]);
    let _ = std::fs::remove_file(&file);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(
        !check.status.success()
            && stderr.contains("cannot #[derive(Default)] for 'Holder'")
            && stderr.contains("Array[Plain, 2]"),
        "{stderr}"
    );
}
