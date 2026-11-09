//! Kata 2052's text arm wrote Rust's
//! `sentence.match_indices(" ").map(|(at, _)| at + 1)` to find each word's
//! start, and was refused "no method 'match_indices' on type 'String'"
//! (B-2026-11-09-57). The review's ruling (docs/library/strings.md) adds
//! `char_indices`, double-ended, and `match_indices`, non-overlapping, left
//! to right, an empty pattern matching at every char boundary and the end.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2052_char_indices_gives_byte_offsets_both_ways() {
    let src = "fn main() {
    let s = \"h\u{e9}llo\".to_string();
    let a: Vec[(i64, char)] = s.char_indices().collect();
    let b: Vec[(i64, char)] = s.as_str().char_indices().rev().take(3).collect();
    println(f\"{a} {b}\");
    for (i, c) in \"ab\".char_indices() {
        println(f\"{i} {c}\");
    }
}
";
    assert_eq!(
        run(src),
        "[(0, h), (1, \u{e9}), (3, l), (4, l), (5, o)] [(5, o), (4, l), (3, l)]\n0 a\n1 b\n"
    );
}

#[test]
fn kata_2052_match_indices_non_overlapping_and_empty_pattern() {
    let src = "fn main() {
    let sentence = \"i love eating burger\".to_string();
    let starts: Vec[i64] = sentence.match_indices(\" \").map(|(at, _)| at + 1).collect();
    let m: Vec[(i64, Str)] = \"aaaa\".match_indices(\"aa\").collect();
    let e: Vec[i64] = \"h\u{e9}\".match_indices(\"\").map(|(i, _)| i).collect();
    let none: Vec[(i64, Str)] = sentence.match_indices(\"xyz\").collect();
    println(f\"{starts} {m} {e} {none.len()}\");
}
";
    assert_eq!(run(src), "[2, 7, 14] [(0, aa), (2, aa)] [0, 1, 3] 0\n");
}
