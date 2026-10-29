//! A struct whose `}` is missing before the next item is reported at its
//! own `{`, once, and the next item parses as itself, instead of a
//! "'struct' is a reserved keyword" at the next item with an `r#struct`
//! hint (B-2026-10-29-17).

#[test]
fn an_unclosed_struct_is_reported_at_its_brace() {
    for next in [
        "struct B {\n    y: i64,\n}",
        "pub fn g() {}",
        "shared struct C {}",
        "impl A {}",
    ] {
        let src = format!(
            "struct A {{\n    x: i64,\n\n{next}\n\nfn main() {{\n    println(\"hi\");\n}}\n"
        );
        let result = karac::parse(&src);
        let messages: Vec<_> = result.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            ["unclosed `{` of struct `A`: its `}` is missing before the item on line 4"],
            "{next}"
        );
        assert_eq!(
            (result.errors[0].span.line, result.errors[0].span.column),
            (1, 10)
        );
        assert!(
            result.program.items.len() >= 3,
            "{next}: the next item and main still parse"
        );
    }
}

#[test]
fn a_keyword_field_name_on_the_fields_line_is_not_taken_for_an_item() {
    // Not first on its line, so this stays the keyword diagnostic.
    let src = "struct A {\n    x: i64, fn: i64,\n}\n";
    let result = karac::parse(src);
    assert!(
        result.errors[0].message.contains("reserved keyword"),
        "{:?}",
        result.errors
    );
    // `impl invariant` and a `pub` field are still a struct's own.
    let src = "struct A {\n    pub x: i64,\n    impl invariant self.x >= 0\n}\n";
    let result = karac::parse(src);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}
