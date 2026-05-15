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

// ── Declaration-form lint: `unsafe extern "ABI" { ... }` ──────────
//
// The block-level `///` doc-comment must contain a `# Safety` markdown
// section explaining the trust contract the importer is asserting on
// the foreign code's behalf. Same `#[allow]` / `#[deny]` mechanics as
// the expression-form lint, but the carrier is `ExternBlock.doc_comment`
// (parsed onto the AST node) instead of a preceding `// Safety:` line
// comment. Slice 5a of the `unsafe extern { }` FFI hardening epic
// (phase-5-diagnostics.md:307).

#[test]
fn test_unsafe_extern_block_with_safety_doc_passes() {
    let diags = lint(
        "/// Wraps the libc string functions.\n\
         ///\n\
         /// # Safety\n\
         ///\n\
         /// Callers must pass valid, NUL-terminated pointers.\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "Expected no diagnostics for block with Safety section, got: {:?}",
        diags
    );
}

#[test]
fn test_unsafe_extern_block_without_doc_warns() {
    let diags = lint(
        "unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Warning);
    assert_eq!(diags[0].lint_name, "undocumented_unsafe");
    assert!(
        diags[0].message.contains("# Safety"),
        "diagnostic should mention `# Safety`: {}",
        diags[0].message
    );
}

#[test]
fn test_unsafe_extern_block_with_unrelated_doc_warns() {
    // A doc comment exists but has no `# Safety` markdown section.
    let diags = lint(
        "/// Imports from libc.\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert_eq!(
        diags.len(),
        1,
        "Doc without Safety section should still warn, got: {:?}",
        diags
    );
}

#[test]
fn test_safety_doc_section_is_case_insensitive() {
    let diags = lint(
        "/// # safety\n\
         /// lowercase header is fine\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "Lowercase `# safety` should pass, got: {:?}",
        diags
    );
}

#[test]
fn test_safety_doc_section_accepts_higher_header_levels() {
    // `## Safety` is the rustdoc convention when nested under a parent.
    let diags = lint(
        "/// Top-level prose.\n\
         ///\n\
         /// ## Safety\n\
         ///\n\
         /// Justification here.\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "`## Safety` should pass, got: {:?}",
        diags
    );
}

#[test]
fn test_unsafe_extern_block_allow_attribute_suppresses() {
    let diags = lint(
        "#[allow(undocumented_unsafe)]\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "#[allow(undocumented_unsafe)] should suppress, got: {:?}",
        diags
    );
}

#[test]
fn test_unsafe_extern_block_deny_attribute_promotes_to_error() {
    let diags = lint(
        "#[deny(undocumented_unsafe)]\n\
         unsafe extern \"C\" {\n\
             fn strlen(s: i64) -> i64;\n\
         }",
    );
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}

#[test]
fn test_multiple_unsafe_extern_blocks_each_checked_independently() {
    // First block has Safety doc; second does not. Only the second warns.
    let diags = lint(
        "/// # Safety\n\
         /// Justified.\n\
         unsafe extern \"C\" {\n\
             fn ok(x: i32) -> i32;\n\
         }\n\
         unsafe extern \"C\" {\n\
             fn missing(x: i32) -> i32;\n\
         }",
    );
    assert_eq!(
        diags.len(),
        1,
        "Expected 1 diagnostic for the second block, got: {:?}",
        diags
    );
}
