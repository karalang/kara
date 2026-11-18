//! Kata 1220's two Rust arms (Count Vowels Permutation).
//!
//! B-2026-11-18-60: `let before: HashMap<char, &str> = [('a',
//! "eiu"), ..].into_iter().collect();`, fixed to a `Map[char, Str]`, was
//! refused with `FromIterator[(char, String)]` and no fix. The literal of
//! `[..].into_iter().collect()` is checked against the collected items, so
//! its string literals are `Str`s and its numbers take their slots' types;
//! a view of a temporary is still refused. The runs are pinned in
//! corpus/design/ok_a_list_literal_collected_keeps_its_str_literals.
//!
//! B-2026-11-18-62: Rust's `const FOLLOWS: [&[usize]; 5] = [&[1],
//! &[0, 2], ..]`, fixed to `Array[Slice[usize], 5]`, drew one borrow error
//! per row about a temporary "which this function owns". It is one error
//! at the type: rows of one length get the `Array` of `Array`s as a fix,
//! and ragged rows are told the table has no constant form.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1220-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn check(tag: &str, src: &str) -> Output {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `src` after one `karac fix`.
fn fixed(tag: &str, src: &str) -> String {
    let (dir, path) = fixture(tag, src);
    let _ = karac().arg("fix").arg(&path).output().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    after
}

#[test]
fn a_collected_list_literal_of_char_and_str_pairs_checks_as_a_map_of_str() {
    let src = "fn main() {\n    let before: Map[char, Str] = [('a', \"eiu\"), ('e', \"ai\")].into_iter().collect();\n    let small: Vec[(char, u8)] = [('a', 200)].into_iter().collect();\n    let names: Vec[Str] = [\"x\"].into_iter().collect();\n    println(f\"{before.len()} {small.len()} {names.len()}\");\n}\n";
    let out = check("collect", src);
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn a_collected_list_literal_that_views_a_temporary_is_still_refused() {
    let src = "fn main() {\n    let v: Vec[Str] = [String.from(\"q\").as_str()].into_iter().collect();\n    println(f\"{v.len()}\");\n}\n";
    let out = check("collect-temp", src);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(
        t.contains("a reference to a temporary outlives its statement"),
        "{t}"
    );
}

#[test]
fn a_collected_number_that_does_not_fit_the_collected_type_is_refused() {
    let src = "fn main() {\n    let v: Vec[(char, u8)] = [('a', 300)].into_iter().collect();\n    println(f\"{v.len()}\");\n}\n";
    let out = check("collect-fit", src);
    assert!(!out.status.success(), "{}", text(&out));
}

#[test]
fn a_constant_table_of_equal_slices_is_fixed_to_an_array_of_arrays() {
    let src = "const PAIRS: Array[Slice[usize], 2] = [[1, 2], [0, 2]];\n\nfn main() {\n    println(f\"{PAIRS[1][1]}\");\n}\n";
    let after = fixed("const-equal", src);
    assert!(
        after.starts_with("const PAIRS: Array[Array[usize, 2], 2] = [[1, 2], [0, 2]];"),
        "{after}"
    );
    let out = check("const-equal-after", &after);
    assert!(out.status.success(), "{}", text(&out));
    let nested = "const ROWS: Slice[Slice[i64]] = [[1, 2], [3, 4], [5, 6]];\n\nfn main() {\n    println(f\"{ROWS[2][0]}\");\n}\n";
    let after = fixed("const-nested", nested);
    assert!(
        after.starts_with("const ROWS: Array[Array[i64, 2], 3] ="),
        "{after}"
    );
}

#[test]
fn a_constant_table_of_ragged_slices_is_one_error_naming_the_lengths() {
    let src = "const FOLLOWS: Array[Slice[usize], 5] = [[1], [0, 2], [0, 1, 3, 4], [2, 4], [0]];\n\nfn main() {\n    println(f\"{FOLLOWS[2].len()}\");\n}\n";
    let out = check("const-ragged", src);
    let t = text(&out);
    assert!(!out.status.success(), "{t}");
    assert!(t.contains("differ in length (1, 2 and 4)"), "{t}");
    assert!(t.contains("build it as an `Array[Vec[usize], 5]`"), "{t}");
    assert!(!t.contains("which this function owns"), "{t}");
    assert!(t.contains("1 error(s) found"), "{t}");
}
