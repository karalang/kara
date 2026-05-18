//! Slice 1+3 of v60 item 37 — `karac query attributes [--tool PREFIX]`
//! collector tests. Pins the silent-accept rule for tool namespaces
//! (slice 1) and the JSON-list collector output (slice 3).

use karac::ast::Program;
use karac::query_attributes::{
    collect_attributes, AttributeQueryFilter, AttributeQueryRecord, AttributeQueryValue,
};

fn parse_program(source: &str) -> Program {
    let parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    parsed.program
}

fn collect(source: &str) -> Vec<AttributeQueryRecord> {
    collect_attributes(&parse_program(source), &AttributeQueryFilter::default())
}

fn collect_with_tool(source: &str, prefix: &str) -> Vec<AttributeQueryRecord> {
    collect_attributes(
        &parse_program(source),
        &AttributeQueryFilter {
            tool_prefix: Some(prefix.to_string()),
        },
    )
}

// ── Multi-segment surface ──────────────────────────────────────────

#[test]
fn collects_simple_tool_attribute_on_function() {
    let records = collect("#[karafmt::skip]\nfn aligned() { }");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, vec!["karafmt", "skip"]);
    assert_eq!(records[0].attached_to, "fn aligned");
    assert!(records[0].args.is_empty());
}

#[test]
fn collects_diagnostic_namespace_attribute() {
    // Compiler-reserved namespace members flow through the same
    // collector (the spec scopes the query to all multi-segment
    // attributes, not just tool namespaces); the `--tool` filter is
    // how a tool focuses on its own surface.
    let records =
        collect("#[diagnostic::on_unimplemented(message: \"missing impl\")]\ntrait Foo { }");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, vec!["diagnostic", "on_unimplemented"]);
    assert_eq!(records[0].attached_to, "trait Foo");
}

#[test]
fn ignores_bare_name_attributes() {
    // `#[derive(Eq)]`, `#[deprecated]`, `#[must_use]`, etc. are the
    // compiler's own bare-name surface — they belong to a different
    // read channel and are excluded from the multi-segment query.
    let records = collect("#[derive(Eq)]\n#[deprecated]\n#[must_use]\nstruct S { x: i64 }");
    assert!(records.is_empty());
}

#[test]
fn collects_three_segment_path() {
    let records = collect("#[a::b::c]\nfn f() { }");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, vec!["a", "b", "c"]);
}

// ── --tool filter ──────────────────────────────────────────────────

#[test]
fn tool_filter_first_segment_match() {
    let src = "#[karafmt::skip]\nfn a() { }\n\
               #[acmecorp::audit]\nfn b() { }\n\
               #[karafmt::indent(width: 4)]\nfn c() { }";
    let karafmt = collect_with_tool(src, "karafmt");
    assert_eq!(karafmt.len(), 2);
    assert!(karafmt.iter().all(|r| r.path[0] == "karafmt"));
    let acme = collect_with_tool(src, "acmecorp");
    assert_eq!(acme.len(), 1);
    assert_eq!(acme[0].path[0], "acmecorp");
}

#[test]
fn tool_filter_excludes_diagnostic_namespace() {
    // `--tool karafmt` does not include `#[diagnostic::*]` —
    // first-segment match is exact.
    let src = "#[diagnostic::on_unimplemented(message: \"x\")]\ntrait T { }\n\
               #[karafmt::skip]\nfn a() { }";
    let records = collect_with_tool(src, "karafmt");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, vec!["karafmt", "skip"]);
}

#[test]
fn tool_filter_unknown_prefix_returns_empty() {
    let records = collect_with_tool("#[karafmt::skip]\nfn f() { }", "no_such_tool");
    assert!(records.is_empty());
}

// ── Arg classification ─────────────────────────────────────────────

#[test]
fn classifies_string_int_bool_path_args() {
    let records = collect(
        "#[acmecorp::audit(\
            level: 9, \
            label: \"strict\", \
            enabled: true, \
            mode: Strict, \
            free_positional\
         )]\nfn f() { }",
    );
    assert_eq!(records.len(), 1);
    let args = &records[0].args;
    assert_eq!(args.len(), 5);

    assert_eq!(args[0].name.as_deref(), Some("level"));
    assert!(matches!(&args[0].value, Some(AttributeQueryValue::Int(9))));

    assert_eq!(args[1].name.as_deref(), Some("label"));
    assert!(matches!(
        &args[1].value,
        Some(AttributeQueryValue::String(s)) if s == "strict"
    ));

    assert_eq!(args[2].name.as_deref(), Some("enabled"));
    assert!(matches!(
        &args[2].value,
        Some(AttributeQueryValue::Bool(true))
    ));

    assert_eq!(args[3].name.as_deref(), Some("mode"));
    assert!(matches!(
        &args[3].value,
        Some(AttributeQueryValue::Path(p)) if p == "Strict"
    ));

    // Positional bare identifier → Path classification (Identifier
    // expr-kind classifies the same as a single-segment Path).
    assert_eq!(args[4].name, None);
    assert!(matches!(
        &args[4].value,
        Some(AttributeQueryValue::Path(p)) if p == "free_positional"
    ));
}

#[test]
fn classifies_complex_expressions_as_other() {
    // Arithmetic / call / struct-literal expressions get `Other`.
    let records = collect("#[acmecorp::audit(level: 1 + 2)]\nfn f() { }");
    let value = records[0].args[0].value.as_ref().unwrap();
    assert!(matches!(value, AttributeQueryValue::Other));
}

// ── attached_to surface ────────────────────────────────────────────

#[test]
fn attached_to_walks_struct_fields() {
    let records = collect(
        "struct S {\n\
         #[karafmt::skip]\n\
         x: i64,\n\
         y: i64,\n\
         }",
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attached_to, "struct S.x");
}

#[test]
fn attached_to_walks_enum_variants() {
    let records = collect(
        "enum E {\n\
         #[karafmt::skip]\n\
         Empty,\n\
         Pair(i64, i64),\n\
         }",
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attached_to, "enum E.Empty");
}

#[test]
fn attached_to_walks_trait_methods() {
    let records = collect(
        "trait T {\n\
         #[karafmt::skip]\n\
         fn m(self) -> i64;\n\
         }",
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attached_to, "trait T.m");
}

#[test]
fn attached_to_walks_impl_blocks_and_methods() {
    let records = collect(
        "struct S { x: i64 }\n\
         #[karafmt::skip]\n\
         impl S {\n\
         #[karafmt::skip]\n\
         fn m(self) -> i64 { 0 }\n\
         }",
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].attached_to, "impl S");
    assert_eq!(records[1].attached_to, "impl S.m");
}

#[test]
fn attached_to_walks_trait_impls() {
    let records = collect(
        "trait T { fn m(self) -> i64; }\n\
         struct S { x: i64 }\n\
         #[karafmt::skip]\n\
         impl T for S { fn m(self) -> i64 { 0 } }",
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attached_to, "impl T for S");
}

#[test]
fn attached_to_uses_const_type_alias_distinct_const_extern_kinds() {
    let records = collect(
        "#[karafmt::skip]\nconst MAX_K: i64 = 1;\n\
         #[karafmt::skip]\ntype Alias = i64;\n\
         #[karafmt::skip]\ndistinct type MyId = i64;",
    );
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].attached_to, "const MAX_K");
    assert_eq!(records[1].attached_to, "type Alias");
    assert_eq!(records[2].attached_to, "distinct MyId");
}

// ── Empty / no-op cases ────────────────────────────────────────────

#[test]
fn empty_program_yields_no_records() {
    let records = collect("");
    assert!(records.is_empty());
}

#[test]
fn program_with_only_bare_attributes_yields_no_records() {
    let records = collect("#[deprecated]\nfn old() { }");
    assert!(records.is_empty());
}

#[test]
fn source_order_preserved_across_items_and_subitems() {
    let records = collect(
        "#[a::first]\nfn one() { }\n\
         struct S {\n\
         #[a::second]\n\
         x: i64,\n\
         }\n\
         #[a::third]\nfn two() { }",
    );
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].path, vec!["a", "first"]);
    assert_eq!(records[1].path, vec!["a", "second"]);
    assert_eq!(records[2].path, vec!["a", "third"]);
}
