//! A defaulted literal binding pushed into a collection and read back by
//! index, through a `ref` view of the collection or directly, gets the
//! same annotation as one read back by a `for` loop (kata 2035's
//! `right[size].push(b)` read as `ys[at]` with `let ys = ref right[n - k]`,
//! B-2026-11-20-11; the `for` arm was B-2026-11-09-15).

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
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
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
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("karac-pushed-view-{name}-{}", std::process::id()))
}

#[test]
fn a_pushed_literal_read_through_a_ref_view_gets_its_annotation() {
    let src = "fn f(nums: Vec[i32]) -> i32 {\n    let mut right = Vec.filled(2, Vec.new());\n    let mut b = 0;\n    b += nums[0];\n    right[1].push(b);\n    let mut best = i32.MAX;\n    let ys = ref right[1];\n    let at = 0;\n    if at < ys.len() {\n        best = best.min((10 - 2 * (nums[1] + ys[at])).abs());\n    }\n    best\n}\n\nfn main() {\n    println(f([3i32, 4i32].to_vec()));\n}";
    let (first, text, clean, out) = mend(&scratch("view"), src);
    assert!(first.contains("declare it `b: i32`"), "{first}");
    assert!(text.contains("let mut b: i32 = 0;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "4\n");
}

#[test]
fn a_pushed_literal_read_back_by_a_direct_index_gets_its_annotation() {
    let src = "fn f(nums: Vec[i32]) -> i32 {\n    let mut right = Vec.filled(2, Vec.new());\n    let mut b = 0;\n    b += nums[0];\n    right[1].push(b);\n    let mut best = i32.MAX;\n    best = best.min(right[1][0] + nums[1]);\n    best\n}\n\nfn main() {\n    println(f([3i32, 4i32].to_vec()));\n}";
    let (first, text, clean, out) = mend(&scratch("direct"), src);
    assert!(first.contains("declare it `b: i32`"), "{first}");
    assert!(text.contains("let mut b: i32 = 0;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "7\n");
}

/// A row of the collection walked by a `for` is one index down: `row[0]`
/// reads the pushed binding.
#[test]
fn a_pushed_literal_read_from_a_walked_row_gets_its_annotation() {
    let src = "fn f(nums: Vec[i32]) -> i32 {\n    let mut right = Vec.filled(1, Vec.new());\n    let mut b = 0;\n    b += nums[0];\n    right[0].push(b);\n    let mut best = i32.MAX;\n    for row in ref right {\n        best = best.min(row[0] * nums[1]);\n    }\n    best\n}\n\nfn main() {\n    println(f([3i32, 4i32].to_vec()));\n}";
    let (first, text, clean, out) = mend(&scratch("row"), src);
    assert!(first.contains("declare it `b: i32`"), "{first}");
    assert!(text.contains("let mut b: i32 = 0;"), "{text}");
    assert!(clean, "{text}");
    assert_eq!(out, "12\n");
}
