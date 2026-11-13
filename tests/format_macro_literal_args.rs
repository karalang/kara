//! A `format!` / `println!` argument whose string or char literal holds a
//! backslash escape, a brace or a colon is carried into the f-string hole as
//! written: a hole balances quotes and lexes the literal's escapes, so
//! `text.matches("\r\n").count()` reads the same there (B-2026-11-13-70).
//! The rewrite used to refuse such an argument, so `karac fix` left a Rust
//! `println!` that only a hand edit could repair; an FTP client counting
//! CRLFs on a data connection hit it.

fn repaired(src: &str) -> String {
    let result = karac::parse(src);
    assert!(
        result
            .errors
            .iter()
            .all(|e| e.message.contains("is Rust syntax")),
        "only macros are reported: {:?}",
        result.errors
    );
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    assert_eq!(edits.len(), result.errors.len(), "every macro has its edit");
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    out
}

#[test]
fn format_macro_arguments_with_escapes_braces_and_colons_are_rewritten() {
    let src = r#"fn g(s: Str) -> String { s.to_string() }
fn main() {
    let text = "a\r\nb\r\n";
    println!("{} CRLF", text.matches("\r\n").count());
    let s = format!("[{}|{}]", g("{"), g("a:b"));
    println(s);
    println!("{}", text.replace("\r\n", "\""));
    let c = 'x';
    println!("{} {}", c == '\'', g("}"));
}
"#;
    let out = repaired(src);
    for want in [
        r#"println(f"{text.matches("\r\n").count()} CRLF");"#,
        r#"let s = f"[{g("{")}|{g("a:b")}]";"#,
        r#"println(f"{text.replace("\r\n", "\"")}");"#,
        r#"println(f"{c == '\''} {g("}")}");"#,
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let reparsed = karac::parse(&out);
    assert!(reparsed.errors.is_empty(), "{:?}", reparsed.errors);
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "2 CRLF\n[{|a:b]\na\"b\"\nfalse }\n");
}

#[test]
fn format_macro_argument_with_a_backslash_outside_a_literal_has_no_fix() {
    // An unterminated escape keeps the literal open to the end of the text.
    let src = "fn main() {\nlet n = 1;\nlet s = format!(\"{}\", n \\ 2);\nprintln(s);\n}\n";
    let result = karac::parse(src);
    assert!(
        result.fix_edits.is_empty() && result.fix_diffs.is_empty(),
        "no fix, got {:?}",
        result.fix_edits
    );
}
