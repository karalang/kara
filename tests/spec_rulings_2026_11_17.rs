//! Review-thread rulings of 2026-11-17 on design.md §3, each pinned here:
//! - ruling 6 (B-2026-11-17-110): a `_` in a number stands only between two
//!   digits and a point is followed by a digit, stricter than Rust; each
//!   misplaced form is an error whose fix writes the legal spelling.
//! - ruling 7 (B-2026-11-17-111): E0003 on `priv` says no raw-identifier
//!   escape names it.

use karac::{check_source, PlaygroundDiagnostic};

/// The errors `karac check` reports for `src`.
fn errors(src: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect()
}

/// `src` with every fix of its diagnostics applied.
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
fn misplaced_digit_separators_and_bare_points_are_errors_with_fixes() {
    for (lit, want, says) in [
        ("1__0", "10", "a `_` stands only between two digits"),
        ("1_", "1", "a `_` stands only between two digits"),
        ("0x_FF", "0xFF", "a `_` stands only between two digits"),
        (
            "1_000_.5",
            "1_000.5",
            "a `_` stands only between two digits",
        ),
        ("1.", "1.0", "a point is followed by a digit"),
        ("1.e5", "1.0e5", "a point is followed by a digit"),
    ] {
        let src = format!("fn main() {{\n    let a = {lit};\n    println(f\"{{a}}\");\n}}\n");
        let es = errors(&src);
        assert_eq!(es.len(), 1, "{lit}: {es:#?}");
        assert!(es[0].message.contains(says), "{lit}: {}", es[0].message);
        assert!(
            es[0].message.contains(&format!("so write `{want}`")),
            "{lit}: {}",
            es[0].message
        );
        let out = fixed(&src);
        assert!(out.contains(&format!("let a = {want};")), "{lit}: {out}");
        assert!(errors(&out).is_empty(), "{lit}: {out}");
    }
}

#[test]
fn a_separator_before_a_type_suffix_stays_legal() {
    let src =
        "fn main() {\n    let a = 1_i64;\n    let b = 2.5E+6_f32;\n    let c = 1_000.5;\n    \
               println(f\"{a} {b} {c}\");\n}\n";
    assert!(errors(src).is_empty(), "{:#?}", errors(src));
}

#[test]
fn priv_as_a_name_says_no_raw_escape_names_it() {
    let es = errors("fn main() {\n    let priv = 1;\n}\n");
    assert!(!es.is_empty());
    assert!(
        es[0]
            .message
            .contains("'priv' is also a structural marker, which no raw-identifier escape names"),
        "{}",
        es[0].message
    );
}
