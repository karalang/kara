//! Rust spellings kata 1058's arms met, each carried by `karac fix` to Kāra.
//!
//! - `s.parse()` whose target only a later use names (`let x: i64 =
//!   s.parse().unwrap();`, a tuple of them collected into `Vec[(i64, i64)]`):
//!   the error waits for that use, and the fix writes the call `i64.parse(s)`.
//! - `n as usize > v.len()` over an `i64` `n`: `len()` is `i64` already, so
//!   the fix drops the cast.
//! - `let k = n as usize;` over an `i64` `n` whose `usize` then mixes with
//!   `i64`: the fix drops the cast (B-2026-11-06-3's remainder).
//! - `v.sort_by_key(|x| Reverse(k))`, Rust's descending sort: the fix writes
//!   `v.sort_by(|a, b| k[b].cmp(k[a]))`.

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
    std::env::temp_dir().join(format!("karac-kata-1058-{name}-{}", std::process::id()))
}

#[test]
fn kata_1058_let_named_parse_target_becomes_the_call() {
    let src = "fn main() {\n    let s = \"2.715\";\n    let (w, f) = s.split_once('.').unwrap();\n    let whole: i64 = w.parse().unwrap();\n    let frac: i64 = f.parse().expect(\"digits\");\n    println(whole * 1000 + frac);\n}";
    let (first, text, clean, out) = mend(&scratch("parse"), src);
    assert!(first.contains("name the type"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let whole: i64 = i64.parse(w).unwrap();")
            && text.contains("let frac: i64 = i64.parse(f).expect(\"digits\");"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "2715", "{text}");
}

#[test]
fn kata_1058_parse_target_named_by_a_collect_becomes_the_call() {
    let src = "fn main() {\n    let prices = [\"0.700\", \"2.800\"];\n    let parsed: Vec[(i64, i64)] = prices\n        .iter()\n        .map(|p| {\n            let (w, f) = p.split_once(\".\").unwrap();\n            (w.parse().unwrap(), f.parse().unwrap())\n        })\n        .collect();\n    println(f\"{parsed}\");\n}";
    let (first, text, clean, out) = mend(&scratch("parse-collect"), src);
    assert!(
        first.contains("only a later use names it (`i64`)"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("(i64.parse(w).unwrap(), i64.parse(f).unwrap())"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[(0, 700), (2, 800)]", "{text}");
}

#[test]
fn kata_1058_parse_without_a_named_target_has_no_fix() {
    let src =
        "fn main() {\n    let s = \"7\";\n    let n = s.parse().unwrap();\n    println(n);\n}";
    let (first, text, clean, _) = mend(&scratch("parse-unnamed"), src);
    assert!(first.contains("name the type"), "{first}");
    assert!(!clean && text.contains("s.parse().unwrap()"), "{text}");
}

#[test]
fn kata_1058_usize_cast_against_a_len_is_dropped() {
    let src = "fn main() {\n    let v = [4, 5, 6];\n    let need = 2;\n    if need < 0 || need as usize > v.len() {\n        println(\"-1\");\n    } else {\n        println(v.len() - need);\n    }\n}";
    let (first, text, clean, out) = mend(&scratch("cmp"), src);
    assert!(first.contains("drop the `as usize`"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("need > v.len()") && !text.contains("usize"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "1", "{text}");
}

#[test]
fn kata_1058_usize_cast_let_that_mixes_is_dropped() {
    let src = "fn main() {\n    let v = [9, 8, 7, 6];\n    let need = 3;\n    let need = need as usize;\n    let up: i64 = v.iter().take(need).sum();\n    println(up + v.len() - need);\n}";
    let (first, text, clean, out) = mend(&scratch("let"), src);
    assert!(first.contains("this `as usize` makes a `usize`"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("let need = need;") && !text.contains("usize"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "25", "{text}");
}

#[test]
fn kata_1058_usize_cast_let_alone_draws_nothing() {
    let src = "fn main() {\n    let n = 3;\n    let k = n as usize;\n    println(k);\n}";
    let (first, text, clean, out) = mend(&scratch("let-alone"), src);
    assert!(!first.contains("as usize"), "{first}");
    assert!(clean && text.contains("let k = n as usize;"), "{text}");
    assert_eq!(out.trim_end(), "3");
}

#[test]
fn kata_1058_reverse_sort_key_becomes_a_descending_sort_by() {
    let src = "use std::cmp::Reverse;\nfn main() {\n    let mut v = [3, 1, 2];\n    v.sort_unstable_by_key(|&x| Reverse(x));\n    let mut w = [(1, 5), (2, 3), (3, 9)];\n    w.sort_by_key(|p| Reverse(p.1 * 2));\n    let mut u = [4, 6, 5];\n    u.sort_by_key(|a| Reverse(*a));\n    println(f\"{v} {w} {u}\");\n}";
    let (first, text, clean, out) = mend(&scratch("sort-key"), src);
    assert!(first.contains("a descending sort compares"), "{first}");
    assert!(!first.contains("Release"), "{first}");
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("v.sort_by(|a, b| b.cmp(a));")
            && text.contains("w.sort_by(|a, b| (b.1 * 2).cmp(a.1 * 2));")
            && text.contains("u.sort_by(|l, r| r.cmp(l));")
            && !text.contains("Reverse"),
        "{text}"
    );
    assert_eq!(
        out.trim_end(),
        "[3, 2, 1] [(3, 9), (1, 5), (2, 3)] [6, 5, 4]",
        "{text}"
    );
}

#[test]
fn kata_1058_reverse_inside_a_larger_key_takes_std_cmp_reverse() {
    // Was `kata_1058_reverse_inside_a_larger_key_has_no_fix`: the library's
    // `std.cmp.Reverse` (B-2026-11-10-41) keeps the key as written.
    let src = "use std::cmp::Reverse;\nfn main() {\n    let mut v = [(1, 3), (1, 2), (0, 5)];\n    v.sort_by_key(|p| (p.0, Reverse(p.1)));\n    println(f\"{v}\");\n}";
    let (first, text, clean, out) = mend(&scratch("sort-key-inner"), src);
    assert!(
        first.contains("Kāra's `Reverse` is `std.cmp.Reverse`"),
        "{first}"
    );
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.starts_with("import std.cmp.Reverse;\n")
            && text.contains("v.sort_by_key(|p| (p.0, Reverse { value: p.1 }));"),
        "{text}"
    );
    assert_eq!(out.trim_end(), "[(0, 5), (1, 3), (1, 2)]", "{text}");
}
