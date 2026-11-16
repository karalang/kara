//! design.md §3 (Lexical structure) and §6 (Operators and precedence)
//! against the lexer and parser, from the conformance pass of 2026-11-16:
//! the escapes every text literal takes, prefixes that do not combine, the
//! line a token spanning lines is reported on, float literals past their
//! type's range, comparisons that do not chain, and one error for one
//! mistake where the parser used to report several or the wrong one.

use karac::{run_playground, PlaygroundDiagnostic, PlaygroundResult};

fn errors(r: &PlaygroundResult) -> Vec<&PlaygroundDiagnostic> {
    r.diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect()
}

/// `src` runs cleanly and prints `want`.
fn runs(src: &str, want: &[&str]) {
    let r = run_playground(src);
    assert!(r.ok, "{src}\n{:#?}", r.diagnostics);
    let want: String = want.iter().map(|l| format!("{l}\n")).collect();
    assert_eq!(r.stdout.concat(), want, "{src}");
}

/// `src` is refused with exactly one error, at `line:column`, whose message
/// contains `msg`.
fn one_error(src: &str, line: usize, column: usize, msg: &str) {
    let r = run_playground(src);
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{src}\n{errs:#?}");
    assert!(errs[0].message.contains(msg), "{src}\n{:#?}", errs[0]);
    assert_eq!((errs[0].line, errs[0].column), (line, column), "{src}");
}

// ── Escapes (B-2026-11-16-336) ─────────────────────────────────────────

#[test]
fn lex_apostrophe_escape_in_every_text_literal() {
    runs(
        "fn main() {\n    println(\"a[\\'] q[\\\"]\");\n    let x = 1;\n    \
         println(f\"{x}[\\']\");\n    let m = \"\"\"\n        it\\'s\n        \"\"\";\n    \
         println(m);\n    println(f\"{c\"\\'\\\"\".len()}\");\n}\n",
        &["a['] q[\"]", "1[']", "it's", "2"],
    );
}

#[test]
fn lex_quote_escape_in_a_char_literal() {
    runs(
        "fn main() {\n    let q = '\\\"';\n    let a = '\\'';\n    println(f\"{q}{a}\");\n}\n",
        &["\"'"],
    );
}

// ── Prefixes (B-2026-11-16-337) ────────────────────────────────────────

#[test]
fn lex_combined_string_prefixes_are_one_focused_error() {
    for p in ["rf", "fr", "br", "rb", "cr", "fb", "bc"] {
        one_error(
            &format!("fn main() {{\n    let s = {p}\"x\\d\";\n    println(\"ok\");\n}}\n"),
            2,
            13,
            &format!("`{p}\"...\"` is not a literal form"),
        );
    }
}

#[test]
fn lex_c_char_and_raw_triple_quote_are_focused_errors() {
    one_error(
        "fn main() {\n    let c = c'a';\n    println(\"ok\");\n}\n",
        2,
        13,
        "there is no `c'...'` literal",
    );
    one_error(
        "fn main() {\n    let s = r\"\"\"x\"\"\";\n    println(\"ok\");\n}\n",
        2,
        13,
        "a raw string has no triple-quoted form",
    );
}

// ── Spans of tokens that span lines (B-2026-11-16-338) ─────────────────

#[test]
fn lex_a_token_spanning_lines_is_reported_where_it_starts() {
    one_error(
        "fn main() {\n    let s = r#\"open\n\n    println(s);\n}\n",
        2,
        13,
        "Unterminated raw string",
    );
    // A multi-line string's type error points at its first line.
    let r = run_playground(
        "fn main() {\n    let n: i64 = \"\"\"\n        a\n        \"\"\";\n    println(f\"{n}\");\n}\n",
    );
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert_eq!((errs[0].line, errs[0].column), (2, 18), "{errs:#?}");
}

// ── Float literals past their type's range (B-2026-11-16-339) ──────────

#[test]
fn lex_float_literal_past_its_range_is_an_error() {
    for (lit, ty) in [
        ("1e400", "'f64'"),
        ("1e39f32", "'f32'"),
        ("65520.0f16", "'f16'"),
    ] {
        one_error(
            &format!("fn main() {{\n    let x = {lit};\n    println(f\"{{x}}\");\n}}\n"),
            2,
            13,
            &format!("float literal `{lit}` is out of range for {ty}"),
        );
    }
    runs(
        "fn main() {\n    let a = 3.4028235e38f32;\n    let b = 65504.0f16;\n    \
         let c = 1.7976931348623157e308;\n    println(f\"{a > 0.0f32} {b > 0.0f16} {c > 0.0}\");\n}\n",
        &["true true true"],
    );
}

// ── Comparisons do not chain (B-2026-11-16-246) ──────

#[test]
fn parse_chained_comparison_is_an_error() {
    for (expr, col) in [
        ("1 == 1 == true", 20),
        ("1 < 2 < 3", 19),
        ("1 < 2 == true", 19),
    ] {
        one_error(
            &format!("fn main() {{\n    let v = {expr};\n    println(f\"{{v}}\");\n}}\n"),
            2,
            col,
            "comparisons do not chain",
        );
    }
    runs(
        "fn main() {\n    let v = (1 < 2) == true;\n    let w = 1 < 2 and 2 < 3;\n    \
         println(f\"{v} {w}\");\n}\n",
        &["true true"],
    );
}

// ── One error per mistake (B-2026-11-16-342) ───────────────────────────

const BEFORE: &str =
    "fn helper(a: i64) -> i64 {\n    a + 1\n}\n\nfn main() {\n    let x = helper(1);\n";
const AFTER: &str =
    "    println(f\"{x}\");\n}\n\nfn other() -> i64 {\n    let y = 2;\n    y * 3\n}\n";

fn one_mistake(line7: &str, line: usize, column: usize, msg: &str) {
    one_error(&format!("{BEFORE}    {line7}\n{AFTER}"), line, column, msg);
}

#[test]
fn parse_missing_fat_arrow_is_one_error() {
    one_mistake(
        "let g = match x { 1 => 2, _ 3 };",
        7,
        33,
        "Expected `=>`, found `3`",
    );
    one_mistake(
        "let g = match x { 1 -> 2, _ => 3 };",
        7,
        25,
        "Expected `=>`, found `->`",
    );
}

#[test]
fn parse_else_without_if_is_one_error() {
    one_mistake("else { println(\"e\"); }", 7, 5, "`else` without an `if`");
}

#[test]
fn parse_missing_close_brace_before_an_item_is_one_error() {
    one_mistake(
        "if x > 0 { println(\"p\");",
        11,
        1,
        "expected `}` before this item: the block opened at 5:11 is not closed",
    );
    // A nested `fn` followed by the rest of its body is still the hoist.
    let r =
        run_playground("fn main() {\nfn helper() -> i64 { 1 }\n    println(f\"{helper()}\");\n}\n");
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(
        errs[0]
            .message
            .contains("a `fn` cannot be declared inside a function body"),
        "{errs:#?}"
    );
}

#[test]
fn lex_unterminated_block_comment_is_one_error_at_its_opening() {
    one_mistake("/* open /* nested */", 7, 5, "Unterminated block comment");
}

#[test]
fn parse_misplaced_module_doc_is_a_focused_error() {
    let r =
        run_playground("//! top\nfn main() {\n    //! inner\n    println(\"x\");\n}\n//! late\n");
    let errs = errors(&r);
    assert_eq!(errs.len(), 2, "{errs:#?}");
    for (e, line) in errs.iter().zip([3, 6]) {
        assert!(
            e.message
                .contains("`//!` documents the module and goes only at the top"),
            "{e:#?}"
        );
        assert_eq!(e.line, line, "{e:#?}");
    }
}

// ── Rename advice that spells a keyword (B-2026-11-16-343) ─────────────

#[test]
fn resolve_case_class_rename_escapes_a_keyword() {
    let r = run_playground("fn main() {\n    let r#Async = 1;\n    println(f\"{r#Async}\");\n}\n");
    let errs = errors(&r);
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(
        errs[0].message.contains("consider renaming to `r#async`"),
        "{errs:#?}"
    );
}

// ── `karac fmt` keeps what it used to drop (B-2026-11-16-344 to B-2026-11-16-347) ──────

fn formats_to(src: &str, want: &str) {
    let once = karac::format_source(src).expect("parses");
    assert_eq!(once, want, "{src}");
    assert_eq!(
        karac::format_source(&once).as_deref(),
        Some(want),
        "idempotent"
    );
}

#[test]
fn fmt_keeps_an_ffi_exports_abi() {
    formats_to(
        "extern \"C\" fn add_one(x: i32) -> i32 { x + 1 }\nfn main() { println(f\"{add_one(41)}\"); }\n",
        "extern \"C\" fn add_one(x: i32) -> i32 {\n    x + 1\n}\n\nfn main() {\n    println(f\"{add_one(41)}\");\n}\n",
    );
}

#[test]
fn fmt_keeps_an_effects_partition_key() {
    formats_to(
        "effect resource Db;\nfn update(id: i64) with writes(Db[id]) { }\nfn main() { update(1); }\n",
        "effect resource Db;\n\nfn update(id: i64) with writes(Db[id]) {}\n\nfn main() {\n    update(1);\n}\n",
    );
}

#[test]
fn fmt_keeps_a_loops_attributes() {
    formats_to(
        "fn main() {\n    let mut i = 0;\n    #[par_order_free]\n    while i < 3 { i = i + 1; }\n    \
         #[par_order_free]\n    for j in 0..2 { println(f\"{j}\"); }\n}\n",
        "fn main() {\n    let mut i = 0;\n    #[par_order_free]\n    while i < 3 {\n        i = i + 1;\n    }\n    \
         #[par_order_free]\n    for j in 0..2 {\n        println(f\"{j}\");\n    }\n}\n",
    );
}

#[test]
fn fmt_keeps_a_c_strings_text() {
    formats_to(
        "fn main() {\n    let a = c\"h\u{e9}llo k\\u{101}ra\";\n    let b = c\"\\xff\\t\";\n    println(f\"{a.len()} {b.len()}\");\n}\n",
        "fn main() {\n    let a = c\"h\u{e9}llo k\u{101}ra\";\n    let b = c\"\\xff\\t\";\n    println(f\"{a.len()} {b.len()}\");\n}\n",
    );
}
