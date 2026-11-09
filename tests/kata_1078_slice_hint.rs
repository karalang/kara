//! Kata 1078, writing the differential's prefix property: `words.slice(0,
//! cut)` for the first `cut` words, the spelling JavaScript gives a run of
//! an array. The library has no `slice` method; a run is the range index
//! `v[a..b]` (collections.md), and the checker said only "no method 'slice'
//! on type 'Vec'". It now names the range index, and `karac fix` writes it
//! when both bounds are an integer literal or a name (B-2026-11-09-17).

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

/// The first check's diagnostics, the text one `karac fix` leaves, whether
/// that text checks clean, and what it prints under the MIR interpreter.
fn one_round(name: &str, src: &str) -> (String, String, bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-1078-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(dir.join("cell.kara")).unwrap();
    let (clean, _, _) = karac(&dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, clean, out)
}

#[test]
fn kata_1078_slice_on_a_vec_is_fixed_to_a_range_index() {
    let src = "fn main() {\n    let words: Vec[String] = [\"a\", \"b\", \"c\", \"d\"].iter().map(|w| w.to_string()).collect();\n    let cut = 3;\n    let first = words.slice(0, cut).to_vec().join(\" \");\n    let arr: Array[i64, 4] = [5, 6, 7, 8];\n    let mid = arr.slice(1, 3).to_vec();\n    println(f\"{first} {mid}\");\n}";
    let (first, text, clean, out) = one_round("fixed", src);
    assert!(
        first.contains("no method 'slice' on type 'Vec[String]': a run of elements is the range index `v[a..b]`"),
        "{first}"
    );
    assert!(first.contains("on type 'Array[i64, 4]'"), "{first}");
    assert!(clean, "still refused after one fix:\n{text}");
    assert!(text.contains("words[0..cut].to_vec()"), "{text}");
    assert!(text.contains("arr[1..3].to_vec()"), "{text}");
    assert_eq!(out.trim_end(), "a b c [6, 7]", "{text}");
}

#[test]
fn kata_1078_slice_with_a_computed_bound_gets_the_hint_alone() {
    let src = "fn main() {\n    let v = [1, 2, 3, 4];\n    let n = v.slice(0, v.len() - 1).len();\n    println(f\"{n}\");\n}";
    let (first, text, clean, _) = one_round("hint", src);
    assert!(
        first.contains(
            "no method 'slice' on type 'Vec[i64]': a run of elements is the range index `v[a..b]`"
        ),
        "{first}"
    );
    assert!(
        !clean,
        "a bound the checker cannot write back has no edit:\n{text}"
    );
    assert!(text.contains("v.slice(0, v.len() - 1)"), "{text}");
}
