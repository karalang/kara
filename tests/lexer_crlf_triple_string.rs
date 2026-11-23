//! A `"""` string in a file with CRLF line endings: the `\r` belongs to the
//! line break, so the value is the same as in an LF file. It used to be
//! refused ("a `"""` string starts on the line after its opening quotes"),
//! which is every `"""` string in a file saved on Windows; CI's Windows job
//! met it through design.md's own example.

use karac::token::{InterpolationPart, Token};
use karac::tokenize;

fn crlf(lf: &str) -> String {
    lf.replace('\n', "\r\n")
}

fn string_of(source: &str) -> String {
    match &tokenize(source)[0].token {
        Token::StringLiteral(s) => s.clone(),
        other => panic!("expected a string literal for {source:?}, got {other:?}"),
    }
}

#[test]
fn crlf_triple_string_has_the_lf_files_value() {
    for lf in [
        "\"\"\"\n    hello\n      world\n    \"\"\"",
        "\"\"\"\n    a\n\n    b\n    \"\"\"",
        "\"\"\"\n\"\"\"",
        "\"\"\"\n  one\n  \"\"\"",
    ] {
        let want = string_of(lf);
        assert!(!want.contains('\r'));
        assert_eq!(string_of(&crlf(lf)), want, "{lf:?}");
    }
    assert_eq!(
        string_of(&crlf("\"\"\"\n    hello\n      world\n    \"\"\"")),
        "hello\n  world"
    );
}

#[test]
fn crlf_interpolated_triple_string_has_the_lf_files_parts() {
    let lf = "f\"\"\"\n    a {x}\n    b\n    \"\"\"";
    let parts = |src: &str| match &tokenize(src)[0].token {
        Token::InterpolatedStringLiteral(p) => p
            .iter()
            .filter_map(|p| match p {
                InterpolationPart::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        other => panic!("expected an f-string for {src:?}, got {other:?}"),
    };
    let want = parts(lf);
    assert_eq!(parts(&crlf(lf)), want);
    assert!(want.concat().contains('\n'));
    assert!(!parts(&crlf(lf)).concat().contains('\r'));
}

#[test]
fn crlf_triple_string_keeps_line_numbers_and_a_lone_cr() {
    // The token after a CRLF block is on the line an editor shows.
    let src = crlf("\"\"\"\n  a\n  b\n  \"\"\"\nlet");
    let toks = tokenize(&src);
    assert_eq!(toks[1].token, Token::Let);
    assert_eq!(toks[1].span.line, 5);
    // A `\r` that is not part of a line break stays in the value.
    assert_eq!(string_of("\"\"\"\n  a\rb\n  \"\"\""), "a\rb");
}
