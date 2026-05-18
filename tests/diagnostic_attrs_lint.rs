//! Slices 3+4 of item 36's `#[diagnostic::*]` attribute namespace entry
//! — `malformed_diagnostic_attribute` lint surface tests for both
//! `#[diagnostic::on_unimplemented(...)]` (trait-only) and
//! `#[diagnostic::do_not_recommend]` (impl-block-only, argument-less).
//!
//! Covers off-target, duplicate, bad-arg-shape, and (for on_unimplemented)
//! unknown-placeholder emission, plus CLI cascade plumbing (`-A`
//! suppresses, `-D` promotes).

use karac::ast::Program;
use karac::diagnostic_attrs_lint::{check_diagnostic_attributes, LintDiagnostic, LintLevel};
use karac::lints::CliLintOverrides;

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

fn lint(source: &str) -> Vec<LintDiagnostic> {
    let prog = parse_program(source);
    check_diagnostic_attributes(&prog, &CliLintOverrides::default())
}

// ── Positive (no warnings) ────────────────────────────────────────

#[test]
fn on_unimpl_lint_positive_trait_with_full_payload() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"m\", label: \"l\", note: \"n\")]\n\
         trait Foo { }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn on_unimpl_lint_positive_trait_with_partial_payload() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"m\")]\ntrait Foo { }");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn on_unimpl_lint_positive_trait_with_self_placeholder() {
    let diags =
        lint("#[diagnostic::on_unimplemented(message: \"{Self} is not Foo\")]\ntrait Foo { }");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn on_unimpl_lint_positive_generic_trait_with_t0_t1_placeholders() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"{Self} cannot map ({T0}, {T1}) -> {T2}\")]\n\
         trait Mapper[A, B, C] { }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// ── Off-target ────────────────────────────────────────────────────

#[test]
fn on_unimpl_lint_off_target_function() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"x\")]\nfn f() { }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("function"));
    assert!(diags[0].message.contains("only valid on `trait`"));
}

#[test]
fn on_unimpl_lint_off_target_struct() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"x\")]\nstruct S { x: i64 }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("struct"));
}

#[test]
fn on_unimpl_lint_off_target_enum() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"x\")]\nenum E { A, B }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("enum"));
}

#[test]
fn on_unimpl_lint_off_target_impl_block() {
    let diags = lint(
        "struct S { x: i64 }\n\
         #[diagnostic::on_unimplemented(message: \"x\")]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("impl block"));
}

#[test]
fn on_unimpl_lint_off_target_const() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"x\")]\nconst MAX_VAL: i64 = 1;");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("module const"));
}

#[test]
fn on_unimpl_lint_off_target_type_alias() {
    let diags = lint("#[diagnostic::on_unimplemented(message: \"x\")]\ntype Alias = i64;");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("type alias"));
}

#[test]
fn on_unimpl_lint_off_target_trait_method() {
    // The attribute names a trait, not a method — the spec scopes
    // it to trait declarations.
    let diags = lint(
        "trait T {\n\
         #[diagnostic::on_unimplemented(message: \"x\")]\n\
         fn method(self) -> i64;\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("trait method"));
}

#[test]
fn on_unimpl_lint_off_target_impl_method() {
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
         #[diagnostic::on_unimplemented(message: \"x\")]\n\
         fn m(self) -> i64 { 0 }\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("impl method"));
}

// ── Duplicate ─────────────────────────────────────────────────────

#[test]
fn on_unimpl_lint_duplicate_emits_one_per_extra() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"first\")]\n\
         #[diagnostic::on_unimplemented(message: \"second\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("duplicate"));
}

#[test]
fn on_unimpl_lint_triplicate_emits_two_warnings() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"first\")]\n\
         #[diagnostic::on_unimplemented(message: \"second\")]\n\
         #[diagnostic::on_unimplemented(message: \"third\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 2);
}

// ── Bad argument shape ────────────────────────────────────────────

#[test]
fn on_unimpl_lint_unknown_field() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"m\", polish_my_error: \"x\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0]
        .message
        .contains("does not accept field `polish_my_error`"));
}

#[test]
fn on_unimpl_lint_typo_field() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(messsage: \"typo\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("`messsage`"));
}

#[test]
fn on_unimpl_lint_non_string_value() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: 42)]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("requires a string-literal value"));
}

#[test]
fn on_unimpl_lint_shorthand_string_value_rejected() {
    let diags = lint("#[diagnostic::on_unimplemented = \"x\"]\ntrait Foo { }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("recognised shape"));
}

#[test]
fn on_unimpl_lint_positional_arg_rejected() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(\"positional\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("requires named arguments"));
}

#[test]
fn on_unimpl_lint_repeated_field_within_attr() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"a\", message: \"b\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("more than once"));
}

// ── Unknown placeholders ──────────────────────────────────────────

#[test]
fn on_unimpl_lint_unknown_placeholder_in_message() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"{NotAParam}\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("unknown placeholder"));
    assert!(diags[0].message.contains("{NotAParam}"));
}

#[test]
fn on_unimpl_lint_t_index_past_arity_warns() {
    // `trait T[A]` has arity 1 → `{T0}` is legal but `{T1}` is not.
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"{T0} and {T1}\")]\n\
         trait T[A] { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("{T1}"));
}

#[test]
fn on_unimpl_lint_t0_on_non_generic_trait_warns() {
    // Arity 0 → `{T0}` is unknown.
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"{T0}\")]\n\
         trait Bare { }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("{T0}"));
}

#[test]
fn on_unimpl_lint_placeholders_checked_in_all_three_fields() {
    let diags = lint(
        "#[diagnostic::on_unimplemented(message: \"{Bad1}\", label: \"{Bad2}\", note: \"{Bad3}\")]\n\
         trait Foo { }",
    );
    assert_eq!(diags.len(), 3);
}

// ── CLI cascade ───────────────────────────────────────────────────

#[test]
fn on_unimpl_lint_cli_allow_suppresses() {
    let cli = CliLintOverrides::with_level(
        "malformed_diagnostic_attribute",
        karac::lints::LintLevel::Allow,
    );
    let prog = parse_program("#[diagnostic::on_unimplemented(message: \"x\")]\nfn f() { }");
    let diags = check_diagnostic_attributes(&prog, &cli);
    assert!(diags.is_empty());
}

#[test]
fn on_unimpl_lint_cli_deny_promotes_to_error() {
    let cli = CliLintOverrides::with_level(
        "malformed_diagnostic_attribute",
        karac::lints::LintLevel::Deny,
    );
    let prog = parse_program("#[diagnostic::on_unimplemented(message: \"x\")]\nfn f() { }");
    let diags = check_diagnostic_attributes(&prog, &cli);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}

#[test]
fn on_unimpl_lint_cli_deny_warnings_promotes_to_error() {
    let cli = CliLintOverrides::with_deny_warnings();
    let prog = parse_program("#[diagnostic::on_unimplemented(message: \"x\")]\nfn f() { }");
    let diags = check_diagnostic_attributes(&prog, &cli);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}

// ── #[diagnostic::do_not_recommend] — slice 4 ─────────────────────

// Positive (no warnings)

#[test]
fn do_not_recommend_lint_positive_on_inherent_impl() {
    let diags = lint(
        "struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn do_not_recommend_lint_positive_on_trait_impl() {
    let diags = lint(
        "trait T { fn m(self) -> i64; }\n\
         struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend]\n\
         impl T for S { fn m(self) -> i64 { 0 } }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// Off-target

#[test]
fn do_not_recommend_lint_off_target_function() {
    let diags = lint("#[diagnostic::do_not_recommend]\nfn f() { }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("function"));
    assert!(diags[0].message.contains("only valid on `impl` blocks"));
}

#[test]
fn do_not_recommend_lint_off_target_struct() {
    let diags = lint("#[diagnostic::do_not_recommend]\nstruct S { x: i64 }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("struct"));
}

#[test]
fn do_not_recommend_lint_off_target_trait() {
    let diags = lint("#[diagnostic::do_not_recommend]\ntrait Foo { }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("trait"));
    assert!(diags[0]
        .message
        .contains("`#[diagnostic::do_not_recommend]`"));
}

#[test]
fn do_not_recommend_lint_off_target_enum() {
    let diags = lint("#[diagnostic::do_not_recommend]\nenum E { A, B }");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("enum"));
}

#[test]
fn do_not_recommend_lint_off_target_impl_method() {
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
         #[diagnostic::do_not_recommend]\n\
         fn m(self) -> i64 { 0 }\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("impl method"));
}

#[test]
fn do_not_recommend_lint_off_target_trait_method() {
    let diags = lint(
        "trait T {\n\
         #[diagnostic::do_not_recommend]\n\
         fn m(self) -> i64;\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("trait method"));
}

// Duplicate

#[test]
fn do_not_recommend_lint_duplicate_emits_one_per_extra() {
    let diags = lint(
        "struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend]\n\
         #[diagnostic::do_not_recommend]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("duplicate"));
}

// Arg-bearing form

#[test]
fn do_not_recommend_lint_paren_form_rejected() {
    let diags = lint(
        "struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend(reason: \"x\")]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("takes no arguments"));
}

#[test]
fn do_not_recommend_lint_string_value_form_rejected() {
    let diags = lint(
        "struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend = \"x\"]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("takes no arguments"));
}

// Parser flag plumbing

#[test]
fn do_not_recommend_lint_sets_ast_flag_on_legal_target() {
    let prog = parse_program(
        "struct S { x: i64 }\n\
         #[diagnostic::do_not_recommend]\n\
         impl S { fn m(self) -> i64 { 0 } }",
    );
    let impl_block = prog
        .items
        .iter()
        .find_map(|it| match it {
            karac::ast::Item::ImplBlock(i) => Some(i),
            _ => None,
        })
        .expect("expected an impl block");
    assert!(impl_block.do_not_recommend);
}

#[test]
fn do_not_recommend_lint_absent_attr_leaves_flag_false() {
    let prog = parse_program("struct S { x: i64 }\nimpl S { fn m(self) -> i64 { 0 } }");
    let impl_block = prog
        .items
        .iter()
        .find_map(|it| match it {
            karac::ast::Item::ImplBlock(i) => Some(i),
            _ => None,
        })
        .expect("expected an impl block");
    assert!(!impl_block.do_not_recommend);
}

// CLI cascade

#[test]
fn do_not_recommend_lint_cli_allow_suppresses() {
    let cli = CliLintOverrides::with_level(
        "malformed_diagnostic_attribute",
        karac::lints::LintLevel::Allow,
    );
    let prog = parse_program("#[diagnostic::do_not_recommend]\nfn f() { }");
    let diags = check_diagnostic_attributes(&prog, &cli);
    assert!(diags.is_empty());
}

#[test]
fn do_not_recommend_lint_cli_deny_promotes_to_error() {
    let cli = CliLintOverrides::with_level(
        "malformed_diagnostic_attribute",
        karac::lints::LintLevel::Deny,
    );
    let prog = parse_program("#[diagnostic::do_not_recommend]\nfn f() { }");
    let diags = check_diagnostic_attributes(&prog, &cli);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}
