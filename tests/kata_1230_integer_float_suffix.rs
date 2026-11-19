//! Kata 1230 (B-2026-11-19-27): Rust's integer-looking float literals,
//! `0f64`, `1f32`, `1_000f64`, `1_f64`, are an error in Kāra, since a
//! literal with neither a point nor an exponent is an integer (design.md
//! §3). The lexer read `0` and left `f64` to the parser as an identifier,
//! whose recovery asked for a `;` after each operand of
//! `vec![vec![0f64; t + 1]; n + 1]`, and `karac fix` applied those,
//! corrupting the line. The literal is now one error whose fix writes the
//! float, and the parse goes on as that float.

use karac::{check_source, PlaygroundDiagnostic};

fn errors(src: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect()
}

fn fixed(src: &str) -> String {
    let mut fixes: Vec<_> = errors(src).into_iter().flat_map(|d| d.fixes).collect();
    fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
    let mut out = src.to_string();
    for f in fixes {
        out.replace_range(f.offset..f.offset + f.length, &f.replacement);
    }
    out
}

#[test]
fn an_integer_with_a_float_suffix_is_one_error_whose_fix_writes_the_float() {
    for (lit, want) in [
        ("0f64", "0.0f64"),
        ("1f32", "1.0f32"),
        ("1_000f64", "1_000.0f64"),
        ("1_f64", "1.0f64"),
    ] {
        let src = format!("fn main() {{\n    let a = {lit};\n    println(f\"{{a}}\");\n}}\n");
        let errs = errors(&src);
        assert_eq!(errs.len(), 1, "{lit}: {errs:?}");
        assert!(
            errs[0]
                .message
                .contains("neither a point nor an exponent is an integer"),
            "{lit}: {}",
            errs[0].message
        );
        let out = fixed(&src);
        assert!(out.contains(&format!("let a = {want};")), "{lit}: {out}");
        assert!(errors(&out).is_empty(), "{lit}: {out}");
    }
}

#[test]
fn a_nested_vec_macro_over_an_integer_float_suffix_is_fixed_without_stray_semicolons() {
    let src = "fn main() {\n    let n = 3;\n    let t = 2;\n    let dp = vec![vec![0f64; t + 1]; n + 1];\n    println(f\"{dp}\");\n}\n";
    let errs = errors(src);
    assert!(
        errs.iter().all(|e| !e.message.contains("expected `;`")),
        "{errs:?}"
    );
    let out = fixed(src);
    assert!(!out.contains(";]"), "{out}");
    assert!(out.contains("0.0f64"), "{out}");
}

#[test]
fn a_float_and_an_integer_suffix_still_lex_as_before() {
    let src = "fn main() {\n    let a = 0.5f32;\n    let b = 7u8;\n    let c = 2.0e3f32;\n    println(f\"{a} {b} {c}\");\n}\n";
    assert!(errors(src).is_empty(), "{:?}", errors(src));
}
