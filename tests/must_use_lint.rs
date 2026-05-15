// tests/must_use_lint.rs
//
// Slice 1 of the `#[must_use]` mandate
// (`docs/implementation_checklist/phase-5-diagnostics.md` § `#[must_use]`
// mandate, slice 1): the two language-level types `Result[T, E]` and
// `Option[T]` are implicitly `#[must_use]`. Discarding a value of either
// type at statement position emits `warning[must_use]` with help / note
// continuation lines.
//
// The tests below pin the walker's discard-site coverage (positive
// cases) and its scoping (negative cases — bindings, tail expressions,
// non-must-use return types).

use karac::ast::Program;
use karac::must_use_lint::{check_implicit_must_use, LintDiagnostic, LintLevel};
use karac::typechecker::TypeCheckResult;

fn parse_and_typecheck(source: &str) -> (Program, TypeCheckResult) {
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
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "Resolve errors: {}",
        resolved
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    (parsed.program, typed)
}

fn lint(source: &str) -> Vec<LintDiagnostic> {
    let (prog, typed) = parse_and_typecheck(source);
    check_implicit_must_use(&prog, Some(&typed))
}

fn assert_must_use_warning(diags: &[LintDiagnostic], needle: &str) {
    assert!(
        diags.iter().any(|d| d.lint_name == "must_use"
            && d.level == LintLevel::Warning
            && d.message.contains(needle)),
        "expected `must_use` warning containing '{needle}', got: {diags:?}"
    );
}

// ── Positive cases (must_use warning fires) ──────────────────────────

#[test]
fn test_discarded_option_call_warns() {
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { produce(); }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Option` value");
}

#[test]
fn test_discarded_result_call_warns() {
    let diags = lint(
        "fn try_it() -> Result[i64, i64] { Result.Ok(7) }\n\
         fn caller() { try_it(); }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Result` value");
}

#[test]
fn test_discarded_option_inside_unsafe_block_warns() {
    // The walker recurses into nested blocks. A discarded Option inside
    // an `unsafe { }` block is still a discarded must-use value — the
    // `unsafe` context controls trust for the *contained operation*,
    // not whether values can be silently dropped.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { unsafe { produce(); } }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Option` value");
}

#[test]
fn test_discarded_in_if_then_branch_warns() {
    // The then-block of an `if` is a nested block; the `;` after the
    // call inside it makes the call a statement-position expression.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller(c: bool) { if c { produce(); } }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Option` value");
}

#[test]
fn test_discarded_in_loop_body_warns() {
    let diags = lint(
        "fn produce() -> Option[i64] { Option.None }\n\
         fn caller() { loop { produce(); break; } }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Option` value");
}

#[test]
fn test_multiple_discards_each_warn() {
    let diags = lint(
        "fn produce() -> Option[i64] { Option.None }\n\
         fn caller() { produce(); produce(); }",
    );
    assert_eq!(
        diags.len(),
        2,
        "expected two warnings (one per discard), got: {diags:?}"
    );
}

#[test]
fn test_discarded_method_call_returning_option_warns() {
    // The lint checks the *return type* of the statement-position
    // expression — the receiver doesn't matter, only the result type
    // recorded by the typechecker.
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S { fn take(self) -> Option[i64] { Option.Some(self.x) } }\n\
         fn caller(s: S) { s.take(); }",
    );
    assert_eq!(diags.len(), 1, "expected one warning, got: {diags:?}");
    assert_must_use_warning(&diags, "discarded `Option` value");
}

// ── Negative cases (must_use warning does NOT fire) ──────────────────

#[test]
fn test_let_binding_does_not_trigger() {
    // `let x = produce();` binds the value; no discard at this site.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { let x = produce(); }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_let_underscore_discard_does_not_trigger() {
    // The canonical explicit-discard form. Slice 1 distinguishes
    // discard-at-statement-position from discard-by-explicit-binding:
    // the former is a hazard, the latter is the author saying "I
    // intentionally drop this".
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { let _ = produce(); }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_tail_expression_does_not_trigger() {
    // The block's `final_expr` flows as the block's value to its
    // consumer (here the function's return). The walker recurses
    // through `final_expr` but does not check it for discard.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() -> Option[i64] { produce() }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_return_does_not_trigger() {
    // `return produce();` — the expression flows out via the return.
    // The Return expression itself is the stmt-position expression and
    // has type `Never`, not `Option`, so it never matches the implicit-
    // must-use type set.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() -> Option[i64] { return produce(); }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_non_must_use_return_type_does_not_trigger() {
    // A discarded i64 call is not a must-use type — slice 1 is scoped
    // to `Result[T, E]` and `Option[T]`. Slice 4 will extend this to
    // user-annotated `#[must_use]` types.
    let diags = lint(
        "fn produce() -> i64 { 7 }\n\
         fn caller() { produce(); }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_method_call_returning_non_must_use_does_not_trigger() {
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { produce().is_some(); }",
    );
    // `produce()` is consumed by `.is_some()` — not discarded at stmt
    // position. The discarded value is `bool` (from `is_some`), which
    // is not implicit must-use.
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_nested_in_let_value_does_not_trigger() {
    // The discarded-at-stmt-position rule is precise: a call appearing
    // as the right-hand side of a `let` is consumed by the binding,
    // even when the binding's pattern would itself discard. Slice 1
    // matches the language semantics, not a textual approximation.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { let _opt = produce(); }",
    );
    assert!(diags.is_empty(), "expected no warnings, got: {diags:?}");
}

#[test]
fn test_let_binding_inside_block_with_discard_warns_once() {
    // Mixed body: one binding (consumed) plus one stmt-position
    // discard. Only the latter warns.
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() {\n\
             let _x = produce();\n\
             produce();\n\
         }",
    );
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one warning, got: {diags:?}"
    );
    assert_must_use_warning(&diags, "discarded `Option` value");
}

// ── Diagnostic shape ────────────────────────────────────────────────

#[test]
fn test_discarded_option_diagnostic_has_help_and_note() {
    let diags = lint(
        "fn produce() -> Option[i64] { Option.Some(7) }\n\
         fn caller() { produce(); }",
    );
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    let help = d
        .help
        .as_ref()
        .expect("must_use diagnostic should carry help");
    assert!(
        help.contains("let _ = "),
        "help should suggest `let _ = ...`, got: {help}"
    );
    assert!(
        help.contains("match") || help.contains("if let"),
        "help should mention pattern-matching alternatives, got: {help}"
    );
    let note = d
        .note
        .as_ref()
        .expect("must_use diagnostic should carry note");
    assert!(
        note.contains("`None` branch"),
        "note should explain why dropping Option is a hazard, got: {note}"
    );
    assert!(
        note.contains("language-level"),
        "note should pin that this is a language-level recognition, got: {note}"
    );
}

#[test]
fn test_discarded_result_diagnostic_has_help_and_note() {
    let diags = lint(
        "fn try_it() -> Result[i64, i64] { Result.Ok(7) }\n\
         fn caller() { try_it(); }",
    );
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    let note = d
        .note
        .as_ref()
        .expect("must_use diagnostic should carry note");
    assert!(
        note.contains("`Err` branch"),
        "note should explain why dropping Result is a hazard, got: {note}"
    );
}
