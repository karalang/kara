//! `karac fmt` and the soft words (B-2026-11-09-52). The formatter decided
//! which names to write as `r#name` from its own copy of the keyword table,
//! which outlived the 2026-10-07 change that made 30 words contextual: kata
//! 1087's `let group = f"..."` came out as `let r#group = ...`, and so did
//! every binding, field or function named `seq`, `resource`, `with`, `stable`,
//! `layout`, `alias` and the rest. The formatter now asks the lexer, so a
//! word is escaped exactly when it would not lex as an identifier.

/// Every word that lexes as an identifier but was in the formatter's list.
/// `own` is left out: it is a structural marker, which `r#` cannot escape.
const SOFT_WORDS: &[&str] = &[
    "union",
    "marker",
    "mod",
    "use",
    "weak",
    "lock",
    "resource",
    "verb",
    "reads",
    "writes",
    "sends",
    "receives",
    "allocates",
    "panics",
    "blocks",
    "suspends",
    "transparent",
    "stable",
    "seq",
    "par",
    "layout",
    "group",
    "alias",
    "independent",
    "distinct",
    "shared",
    "where",
    "with",
    "effect",
    "requires",
    "ensures",
    "invariant",
    "providers",
];

fn program(w: &str) -> String {
    format!(
        "struct Holder {{
    {w}: i64,
}}

fn twice({w}: i64) -> i64 {{
    {w} * 2
}}

fn main() {{
    let {w} = 1;
    let h = Holder {{
        {w}: twice({w}),
    }};
    println(f\"{{h.{w}}}\");
}}
"
    )
}

#[test]
fn fmt_writes_a_soft_word_bare() {
    for w in SOFT_WORDS {
        let src = program(w);
        let once = karac::format_source(&src)
            .unwrap_or_else(|| panic!("`{w}`: the program does not parse:\n{src}"));
        assert!(!once.contains("r#"), "`{w}` came out escaped:\n{once}");
        assert_eq!(once, src, "`{w}`: fmt changed the program");
    }
}

#[test]
fn fmt_drops_a_redundant_escape_on_a_soft_word() {
    // corpus/fixtures/contextual_keywords/raw_escape_of_a_soft_word_is_still_accepted
    let src = "fn main() { let r#union = 1; let r#reads = r#union + 1; }";
    let once = karac::format_source(src).unwrap();
    assert_eq!(
        once,
        "fn main() {\n    let union = 1;\n    let reads = union + 1;\n}\n"
    );
    assert_eq!(karac::format_source(&once).as_deref(), Some(once.as_str()));
}

#[test]
fn fmt_still_escapes_a_keyword_and_a_reserved_word() {
    let src = "fn main() {\n    let r#match = 1;\n    let r#async = r#match + 1;\n    let r#try = r#async;\n    println(f\"{r#try}\");\n}\n";
    let once = karac::format_source(src).unwrap();
    assert_eq!(once, src);
}
