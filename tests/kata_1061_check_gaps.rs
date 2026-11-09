//! Spellings kata 1061's arms met, each carried by `karac fix` to a clean
//! check.
//!
//! - `s1.bytes().zip(s2.bytes())`: a `Slice` is not an `Iterator`, and the fix
//!   wrote `.iter()` on the receiver but nothing on the argument, which has
//!   no `into_iter` to suggest. The argument now gets the `.iter()` too.
//! - `find(mut parent, (a - b'a') as i64)`: `lossless_cast` had no fix for a
//!   parenthesized operand, whose node ends before its `)`. The fix now
//!   deletes the `as i64` past the `)` and keeps the parentheses.
//! - Rust's nested `fn` inside a method body read as a misplaced identifier
//!   (`'fn' is a reserved keyword ... write r#fn`). It is now refused as a
//!   nested item, and the fix moves it, dedented, above the item it was in.
//! - `(b[i] - b'a') as usize` passed where an `i64` is wanted, once the
//!   `usize` parameters are `i64`: the fix casts to `i64`, which the lossless
//!   cast lint then drops.
//! - `set.first()` on a `SortedSet` named Kāra's `min()` with no fix; the fix
//!   now renames it (and `first_key_value` and the rest of its twins).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean and what the last check says, and what it prints
/// under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..4 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, last) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, last, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-kata-1061-{name}-{}", std::process::id()))
}

#[test]
fn kata_1061_slice_zip_argument_gets_iter() {
    let src = "fn main() {\n    let s1 = \"abc\";\n    let s2 = \"bcd\";\n    let mut n = 0;\n    for (a, b) in s1.bytes().zip(s2.bytes()) {\n        if a < b {\n            n += 1;\n        }\n    }\n    println(n);\n}";
    let (first, text, clean, last, out) = mend(&scratch("zip"), src);
    assert!(first.contains("iterate it with `.iter()`"), "{first}");
    assert!(
        text.contains("s1.bytes().iter().zip(s2.bytes().iter())"),
        "{text}"
    );
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "3\n");
}

#[test]
fn kata_1061_parenthesized_lossless_cast_is_dropped() {
    let src = "fn index(i: i64) -> i64 {\n    i * 2\n}\n\nfn main() {\n    let c = b'd';\n    let a = index((c - b'a') as i64);\n    let b: i64 = (( c - b'b' )) as i64;\n    println(f\"{a} {b}\");\n}";
    let (first, text, clean, last, out) = mend(&scratch("cast"), src);
    assert!(first.contains("widens to 'i64' implicitly"), "{first}");
    assert!(text.contains("index((c - b'a'));"), "{text}");
    assert!(text.contains("let b: i64 = (( c - b'b' ));"), "{text}");
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "6 2\n");
}

#[test]
fn kata_1061_narrowing_cast_keeps_its_as() {
    let src = "fn main() {\n    let x = 300;\n    let y = (x + 1) as u8;\n    println(y);\n}";
    let (first, text, _, _, _) = mend(&scratch("narrow"), src);
    assert!(!first.contains("unnecessary cast"), "{first}");
    assert!(text.contains("(x + 1) as u8"), "{text}");
}

#[test]
fn kata_1061_nested_fn_moves_to_the_top_level() {
    let src = "struct Solution {}\n\nimpl Solution {\n    fn run(n: i64) -> i64 {\n        fn twice(x: i64) -> i64 {\n            x * 2\n        }\n        twice(n) + 1\n    }\n}\n\nfn main() {\n    println(Solution.run(4));\n}";
    let (first, text, clean, last, out) = mend(&scratch("nested"), src);
    assert!(
        first.contains("cannot be declared inside a function body"),
        "{first}"
    );
    assert!(
        text.contains("struct Solution {}\n\nfn twice(x: i64) -> i64 {\n    x * 2\n}\n\nimpl Solution {\n    fn run(n: i64) -> i64 {\n        twice(n) + 1\n"),
        "{text}"
    );
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "9\n");
}

#[test]
fn kata_1061_usize_cast_where_i64_is_wanted_becomes_i64() {
    let src = "fn find(parent: Vec<usize>, x: usize) -> usize {\n    parent[x]\n}\n\nfn main() {\n    let parent: Vec<usize> = (0..26).collect();\n    let s = \"dog\";\n    let b = s.as_bytes();\n    println(find(parent, (b[0] - b'a') as usize));\n}";
    let (first, text, clean, last, out) = mend(&scratch("usize"), src);
    // `(0..26)` collects as the `Vec[usize]` it is annotated (design.md §
    // Type inference, B-2026-11-08-86), so the cast is only a lossless one
    // that the fix drops.
    assert!(!first.contains("FromIterator"), "{first}");
    assert!(
        first.contains("unnecessary cast: 'u8' widens to 'usize'"),
        "{first}"
    );
    assert!(text.contains("find(parent, (b[0] - b'a'))"), "{text}");
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "3\n");
}

#[test]
fn kata_1061_sorted_set_first_is_renamed_min() {
    let src = "fn main() {\n    let mut s: SortedSet[char] = SortedSet.new();\n    s.insert('q');\n    s.insert('c');\n    let m: SortedMap[i64, i64] = SortedMap.new();\n    println(f\"{s.first().unwrap()} {s.last().unwrap()} {m.first_key_value().is_none()}\");\n}";
    let (first, text, clean, last, out) = mend(&scratch("twin"), src);
    assert!(first.contains("Kāra's is `min()`"), "{first}");
    assert!(
        text.contains("{s.min().unwrap()} {s.max().unwrap()} {m.first().is_none()}"),
        "{text}"
    );
    assert!(clean && !last.contains("warning"), "{text}\n{last}");
    assert_eq!(out, "c q true\n");
}
