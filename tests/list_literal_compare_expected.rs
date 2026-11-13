//! The other operand of `==` or `!=` is an expected type for a list
//! literal, as it is for a number literal (design.md § Collection literals,
//! ruling 60cc717ee6): with `m: Array[u8, 4]`, `m != [75, 82, 69, 67]`
//! builds an `Array[u8, 4]`, and with `s: Set[i64]`, `s == [1, 2]` builds a
//! `Set`. The literal was typed `Vec` on its own, so both were refused as
//! "cannot compare 'Array[u8, 4]' and 'Vec[u8]'" (B-2026-11-13-2, found
//! writing a magic-number check in a binary record decoder).

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
        "karac-list-literal-compare-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = temp(tag, src);
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Both spellings (suffixed and unsuffixed elements) and both operand
/// orders, and through a borrow of the array.
#[test]
fn a_list_literal_compared_with_an_array_builds_that_array() {
    runs_to(
        "array",
        "fn main() {\n    let m: Array[u8, 4] = [75u8, 82u8, 69u8, 67u8];\n    \
         println(m == [75, 82, 69, 67]);\n    \
         println(m != [75u8, 82u8, 69u8, 68u8]);\n    \
         println([75, 82, 69, 67] == m);\n    \
         println([75u8, 82u8, 69u8, 67u8] != m);\n    \
         let r = ref m;\n    println(r == [75, 82, 69, 67]);\n}\n",
        "true\ntrue\ntrue\nfalse\ntrue\n",
    );
}

#[test]
fn a_list_literal_compared_with_a_set_builds_a_set() {
    runs_to(
        "set",
        "fn main() {\n    let mut s: Set[i64] = Set.new();\n    s.insert(1);\n    s.insert(2);\n    \
         println(s == [2, 1]);\n    println([1, 3] != s);\n    println(s == [1]);\n}\n",
        "true\ntrue\nfalse\n",
    );
}

/// A `Vec` of a narrower element takes its type too (`Vec[i32]` against
/// `[1, 2]` was "cannot compare 'Vec[i32]' and 'Vec[i64]'").
#[test]
fn a_list_literal_compared_with_a_narrow_vec_takes_its_element_type() {
    runs_to(
        "vec",
        "fn main() {\n    let v: Vec[i32] = [1, 2];\n    println(v == [1, 2]);\n    \
         println([1, 3] != v);\n    let f: Vec[f32] = [1.5];\n    println(f == [1.5]);\n}\n",
        "true\ntrue\ntrue\n",
    );
}

/// Checked against the array, a literal of the wrong length or with an
/// element out of range is refused for that, not as a `Vec`.
#[test]
fn a_list_literal_against_an_array_is_checked_as_that_array() {
    let src = "fn main() {\n    let m: Array[u8, 4] = [75u8, 82u8, 69u8, 67u8];\n    \
               println(m == [1, 2, 3]);\n    println(m == [300, 1, 2, 3]);\n}\n";
    let path = temp("refused", src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "checked clean");
    assert!(
        stderr.contains("array literal has 3 element(s), expected 4")
            && stderr.contains("integer literal 300 out of range for 'u8'")
            && !stderr.contains("cannot compare"),
        "{stderr}"
    );
}
