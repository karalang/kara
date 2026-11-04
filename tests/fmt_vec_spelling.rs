//! `karac fmt` prints a `Vec` literal as it was written: `vec![..]` stays
//! (the deprecated spelling is `karac fix`'s to rewrite, design.md §
//! Collection literals), and `Vec[..]` or `[..]` never becomes `vec![..]`,
//! in receiver position too, where a prefix literal is parenthesized.

#[test]
fn fmt_keeps_each_vec_literal_spelling() {
    let src = "fn main() {\n    let a = vec![1, 2];\n    let b = Vec[3, 4];\n    let c = [5, 6];\n    \
               let n = 2;\n    let d = vec![7; n];\n    let e: Vec[i64] = vec![];\n    \
               let f = Vec[1, 2].len();\n    let g = vec![1, 2].len();\n    let h = [1, 2].len();\n    \
               println(f\"{a} {b} {c} {d} {e} {f} {g} {h}\");\n}\n";
    let once = karac::format_source(src).expect("formats");
    for want in [
        "let a = vec![1, 2];",
        "let b = Vec[3, 4];",
        "let c = [5, 6];",
        "let d = vec![7; n];",
        "let e: Vec[i64] = vec![];",
        "let f = (Vec[1, 2]).len();",
        "let g = vec![1, 2].len();",
        "let h = [1, 2].len();",
    ] {
        assert!(once.contains(want), "missing {want:?} in:\n{once}");
    }
    assert_eq!(
        karac::format_source(&once).as_deref(),
        Some(once.as_str()),
        "a second format changes it"
    );
    let deprecated = |s: &str| {
        karac::check_source(s)
            .iter()
            .filter(|d| d.message.starts_with("use of deprecated `vec![..]`"))
            .count()
    };
    assert_eq!(deprecated(src), 4);
    assert_eq!(deprecated(&once), 4);
}
