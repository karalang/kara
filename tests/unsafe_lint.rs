use karac::ast::Program;
use karac::unsafe_lint::{check_undocumented_unsafe, LintLevel};

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

fn lint(source: &str) -> Vec<karac::unsafe_lint::LintDiagnostic> {
    let prog = parse_program(source);
    check_undocumented_unsafe(&prog, source)
}

#[test]
fn test_unsafe_with_safety_comment_passes() {
    let diags = lint(
        "fn f() {\n\
         // Safety: we checked the pointer above\n\
         unsafe { }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "Expected no diagnostics, got: {:?}",
        diags
    );
}

#[test]
fn test_unsafe_without_comment_warns() {
    let diags = lint("fn f() {\n    unsafe { }\n}");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Warning);
    assert_eq!(diags[0].lint_name, "undocumented_unsafe");
}

#[test]
fn test_unsafe_with_unrelated_comment_warns() {
    let diags = lint(
        "fn f() {\n\
         // This does something\n\
         unsafe { }\n\
         }",
    );
    assert_eq!(diags.len(), 1, "Expected 1 diagnostic, got: {:?}", diags);
    assert_eq!(diags[0].level, LintLevel::Warning);
}

#[test]
fn test_safety_comment_case_insensitive() {
    let diags = lint(
        "fn f() {\n\
         // safety: lowercase is fine\n\
         unsafe { }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "Lowercase safety: should pass, got: {:?}",
        diags
    );
}

#[test]
fn test_safety_comment_with_text_after_colon() {
    // "Safety:" must be followed by text — just having "safety:" prefix is enough
    let diags = lint(
        "fn f() {\n\
         // Safety: pointer is valid because it comes from Box::into_raw\n\
         unsafe { }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "Safety: with text should pass, got: {:?}",
        diags
    );
}

#[test]
fn test_allow_attribute_suppresses() {
    let diags = lint(
        "#[allow(undocumented_unsafe)]\n\
         fn f() {\n\
             unsafe { }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "allow attribute should suppress, got: {:?}",
        diags
    );
}

#[test]
fn test_unsafe_at_line_1_warns() {
    // unsafe on the first line — no preceding line to hold Safety:
    let diags = lint("fn f() { unsafe { } }");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Warning);
}

#[test]
fn test_multiple_unsafe_blocks_each_checked() {
    let diags = lint(
        "fn f() {\n\
         // Safety: first\n\
         unsafe { }\n\
         unsafe { }\n\
         }",
    );
    // First has Safety:, second doesn't
    assert_eq!(
        diags.len(),
        1,
        "Expected 1 diagnostic for second block, got: {:?}",
        diags
    );
}
