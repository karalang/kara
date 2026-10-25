//! A token the lexer cannot read reaches the user as the lexer's own
//! message, once, at the token (B-2026-10-24-27): not wrapped as the
//! parser's `Expected expression, found Error("…")` with its quotes
//! Debug-escaped, and with lexing going on after the bad literal, so no
//! cascade follows it. An escaped quote inside an f-string hole carries the
//! fix its writer meant.

use karac::parse;

fn messages(src: &str) -> Vec<String> {
    parse(src).errors.into_iter().map(|e| e.message).collect()
}

#[test]
fn a_lex_error_is_reported_as_its_own_message_once() {
    let cases = [
        (
            "fn main() {\n    let a = \"x\\qy\";\n    println(a);\n}\n",
            "Unknown escape sequence: \\q",
        ),
        (
            "fn main() {\n    let a = b\"x\\qy\";\n    let n = 1;\n}\n",
            "Unknown escape sequence in byte literal: \\q",
        ),
        (
            "fn main() {\n    let a = c\"x\\qy\";\n    let n = 1;\n}\n",
            "Unknown escape sequence in C-string: \\q",
        ),
        (
            "fn main() {\n    let a = \"\\u{110000}x\";\n    let n = 1;\n}\n",
            "",
        ),
    ];
    for (src, want) in cases {
        let got = messages(src);
        assert_eq!(got.len(), 1, "{src}: {got:?}");
        assert!(!got[0].contains("Error("), "{src}: {got:?}");
        assert!(!got[0].starts_with("Expected"), "{src}: {got:?}");
        assert!(got[0].contains(want), "{src}: {got:?}");
    }
}

#[test]
fn an_unterminated_string_still_reports() {
    let got = messages("fn main() {\n    let a = \"abc;\n}\n");
    assert_eq!(got, ["Unterminated string"]);
}

#[test]
fn escaped_quotes_in_an_fstring_hole_carry_the_fix() {
    let src = "fn main() {\n    let v = [\"a\", \"b\"].to_vec();\n    \
               println(f\"{v.join(\\\", \\\")} and {{x}} \\\"q\\\"\");\n}\n";
    let r = parse(src);
    let got: Vec<&str> = r.errors.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].starts_with("unexpected '\\' in f-string interpolation"));
    assert!(!got[0].contains("Error("), "{got:?}");
    let span = r.errors[0].span;
    let edit = &r.fix_edits[&karac::resolver::SpanKey::from_span(&span)];
    // The hole's quotes lose their backslashes; the text's own `\"` and the
    // `{{` stay.
    assert_eq!(
        edit.replacement,
        "f\"{v.join(\", \")} and {{x}} \\\"q\\\"\""
    );
    let mut fixed = src.to_string();
    fixed.replace_range(edit.offset..edit.offset + edit.length, &edit.replacement);
    assert!(parse(&fixed).errors.is_empty(), "{fixed}");
}
