use karac::ast::Program;
use karac::typechecker::TypeCheckResult;
use karac::unsafe_lint::{
    check_undocumented_unsafe, check_unsafe_op_in_unsafe_fn, LintDiagnostic, LintLevel,
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

fn lint(source: &str) -> Vec<karac::unsafe_lint::LintDiagnostic> {
    let prog = parse_program(source);
    check_undocumented_unsafe(&prog, source, &karac::lints::CliLintOverrides::default())
}

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

fn lint_op(source: &str) -> Vec<LintDiagnostic> {
    let (prog, typed) = parse_and_typecheck(source);
    check_unsafe_op_in_unsafe_fn(&prog, Some(&typed))
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

// ── Declaration-form lint: `unsafe fn` (free fn + impl method) ────
//
// An `unsafe fn` declares a precondition the *caller* must uphold, so the
// contract must be written down: every `unsafe fn` needs a `///` doc-comment
// carrying a `# Safety` markdown section (same carrier / matcher as the
// `unsafe extern` block form). Peer of Rust's `clippy::missing_safety_doc`.
// Scoped to user-authored fns; a safe fn is never checked.

#[test]
fn test_unsafe_fn_with_safety_doc_passes() {
    let diags = lint(
        "/// Reads a raw slot.\n\
         ///\n\
         /// # Safety\n\
         ///\n\
         /// `i` must be in bounds for the backing buffer.\n\
         unsafe fn raw(i: i64) -> i64 { i }",
    );
    assert!(
        diags.is_empty(),
        "unsafe fn with a `# Safety` section should pass, got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_without_doc_warns() {
    let diags = lint("unsafe fn raw(i: i64) -> i64 { i }");
    assert_eq!(diags.len(), 1, "expected 1 diagnostic, got: {diags:?}");
    assert_eq!(diags[0].level, LintLevel::Warning);
    assert_eq!(diags[0].lint_name, "undocumented_unsafe");
    assert!(
        diags[0].message.contains("# Safety"),
        "diagnostic should mention `# Safety`: {}",
        diags[0].message
    );
    assert!(
        diags[0].message.contains("raw"),
        "diagnostic should name the offending fn: {}",
        diags[0].message
    );
}

#[test]
fn test_unsafe_fn_with_unrelated_doc_warns() {
    // A doc comment exists but has no `# Safety` markdown section.
    let diags = lint(
        "/// Reads a raw slot.\n\
         unsafe fn raw(i: i64) -> i64 { i }",
    );
    assert_eq!(
        diags.len(),
        1,
        "doc without a Safety section should still warn, got: {diags:?}"
    );
    assert_eq!(diags[0].level, LintLevel::Warning);
}

#[test]
fn test_safe_fn_not_checked_for_safety_doc() {
    // The declaration-doc rule fires only on `unsafe fn` — a plain fn with
    // no doc comment is fine.
    let diags = lint("fn plain(i: i64) -> i64 { i }");
    assert!(
        diags.is_empty(),
        "a safe fn should never require a `# Safety` doc, got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_safety_doc_higher_header_level_passes() {
    // `## Safety` is the rustdoc convention when nested under a parent.
    let diags = lint(
        "/// Top-level prose.\n\
         ///\n\
         /// ## Safety\n\
         ///\n\
         /// Justification here.\n\
         unsafe fn raw() {}",
    );
    assert!(diags.is_empty(), "`## Safety` should pass, got: {diags:?}");
}

#[test]
fn test_unsafe_fn_allow_attribute_suppresses() {
    let diags = lint(
        "#[allow(undocumented_unsafe)]\n\
         unsafe fn raw() {}",
    );
    assert!(
        diags.is_empty(),
        "#[allow(undocumented_unsafe)] should suppress, got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_deny_attribute_promotes_to_error() {
    let diags = lint(
        "#[deny(undocumented_unsafe)]\n\
         unsafe fn raw() {}",
    );
    assert_eq!(diags.len(), 1, "expected 1 diagnostic, got: {diags:?}");
    assert_eq!(diags[0].level, LintLevel::Error);
}

#[test]
fn test_impl_unsafe_method_without_doc_warns() {
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
             unsafe fn raw_read(self) -> i64 { self.x }\n\
         }",
    );
    assert_eq!(diags.len(), 1, "expected 1 diagnostic, got: {diags:?}");
    assert_eq!(diags[0].level, LintLevel::Warning);
    assert!(
        diags[0].message.contains("raw_read"),
        "diagnostic should name the method: {}",
        diags[0].message
    );
}

#[test]
fn test_impl_unsafe_method_with_doc_passes() {
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
             /// # Safety\n\
             /// The caller guarantees `self` is initialized.\n\
             unsafe fn raw_read(self) -> i64 { self.x }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "documented impl method should pass, got: {diags:?}"
    );
}

#[test]
fn test_impl_safe_method_not_checked() {
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
             fn read(self) -> i64 { self.x }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "a safe impl method should never require a `# Safety` doc, got: {diags:?}"
    );
}

#[test]
fn test_impl_unsafe_method_per_method_allow_suppresses() {
    // The per-method `#[allow]` is the innermost cascade level and wins.
    let diags = lint(
        "struct S { x: i64 }\n\
         impl S {\n\
             #[allow(undocumented_unsafe)]\n\
             unsafe fn raw_read(self) -> i64 { self.x }\n\
         }",
    );
    assert!(
        diags.is_empty(),
        "per-method #[allow] should suppress, got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_declaration_and_inner_block_both_fire() {
    // The declaration-doc rule (form 3) and the body-block rule (form 1)
    // are independent — an undocumented `unsafe fn` whose body also has an
    // undocumented `unsafe { }` block produces both diagnostics.
    let diags = lint(
        "unsafe fn raw() {\n\
             unsafe { }\n\
         }",
    );
    assert_eq!(
        diags.len(),
        2,
        "expected the declaration-doc warning AND the inner-block warning, got: {diags:?}"
    );
    assert!(
        diags.iter().all(|d| d.lint_name == "undocumented_unsafe"),
        "both should be undocumented_unsafe, got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_doc_diag_has_no_help_or_note() {
    // `undocumented_unsafe` emits single-line diagnostics (help/note are
    // scoped to `unsafe_op_in_unsafe_fn`) — the form-3 emission matches.
    let diags = lint("unsafe fn raw() {}");
    assert_eq!(diags.len(), 1, "expected 1 diagnostic, got: {diags:?}");
    assert!(diags[0].help.is_none(), "got help: {:?}", diags[0].help);
    assert!(diags[0].note.is_none(), "got note: {:?}", diags[0].note);
}

#[test]
fn test_cli_deny_promotes_unsafe_fn_doc() {
    // The CLI `-D undocumented_unsafe` fall-through reaches the form-3
    // declaration check too.
    let source = "unsafe fn raw() {}";
    let prog = parse_program(source);
    let cli = karac::lints::CliLintOverrides::with_level(
        "undocumented_unsafe",
        karac::lints::LintLevel::Deny,
    );
    let diags = check_undocumented_unsafe(&prog, source, &cli);
    assert_eq!(diags.len(), 1, "expected 1 diagnostic, got: {diags:?}");
    assert_eq!(diags[0].level, LintLevel::Error);
}

// ── Slice 3: `unsafe_op_in_unsafe_fn` operation lint ─────────────────
//
// The lint walks every fn body, tracking whether the cursor is inside an
// `unsafe { ... }` block. Outside any such block, raw-pointer deref and
// calls to `unsafe fn` are hard errors. Inside, they are accepted. The
// rule applies uniformly inside `unsafe fn` bodies — declaring a fn
// `unsafe` is a precondition for *callers*, not an implicit body wrap.

fn assert_unsafe_op_diag(diags: &[LintDiagnostic], needle: &str) {
    assert!(
        diags.iter().any(|d| d.lint_name == "unsafe_op_in_unsafe_fn"
            && d.level == LintLevel::Error
            && d.message.contains(needle)),
        "expected `unsafe_op_in_unsafe_fn` error containing '{needle}', got: {diags:?}"
    );
}

#[test]
fn test_unsafe_fn_call_outside_unsafe_block_errors() {
    let diags = lint_op(
        "unsafe fn raw() {}\n\
         fn caller() { raw(); }",
    );
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "call to `unsafe fn raw`");
}

#[test]
fn test_unsafe_fn_call_inside_unsafe_block_accepted() {
    let diags = lint_op(
        "unsafe fn raw() {}\n\
         fn caller() { unsafe { raw(); } }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_unsafe_fn_body_still_requires_inner_unsafe_block() {
    // The KEY semantic check: `unsafe fn` declares a precondition for
    // callers, it does NOT implicitly wrap its body. Calling another
    // `unsafe fn` from inside an `unsafe fn` body still requires the
    // explicit `unsafe { ... }` wrap.
    let diags = lint_op(
        "unsafe fn raw_a() {}\n\
         unsafe fn raw_b() { raw_a(); }",
    );
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "call to `unsafe fn raw_a`");
}

#[test]
fn test_plain_fn_call_does_not_trigger() {
    let diags = lint_op(
        "fn safe(x: i64) -> i64 { x }\n\
         fn caller() { safe(7); }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_extern_fn_call_does_not_trigger() {
    // The trust boundary is the `unsafe extern { }` block itself, not
    // each call site. Calling an imported extern fn requires no wrap.
    let diags = lint_op(
        "unsafe extern \"C\" { fn libc_strlen(s: i64) -> i64; }\n\
         fn caller() -> i64 { libc_strlen(0) }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_raw_pointer_deref_outside_unsafe_errors() {
    let diags = lint_op("fn caller(p: *const i64) -> i64 { *p }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "raw-pointer dereference");
}

#[test]
fn test_raw_pointer_deref_inside_unsafe_accepted() {
    let diags = lint_op("fn caller(p: *const i64) -> i64 { unsafe { *p } }");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_mut_raw_pointer_deref_outside_unsafe_errors() {
    // `*mut T` is just as unsafe as `*const T` — the rule is symmetric.
    let diags = lint_op("fn caller(p: *mut i64) -> i64 { *p }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "raw-pointer dereference");
}

#[test]
fn test_ref_deref_does_not_trigger() {
    // `*r` on a `ref T` / `mut ref T` is NOT a raw-pointer deref — the
    // lint must not fire on safe references.
    let diags = lint_op("fn read(r: ref i64) -> i64 { *r }");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// ── Raw-pointer INSTANCE methods require `unsafe` (design.md § raw pointers:
//    "Construction is safe; dereference is unsafe" — reads/writes/arithmetic
//    through the pointer all require it). Peer of the `*p` deref rule above.

#[test]
fn test_raw_pointer_read_method_outside_unsafe_errors() {
    let diags = lint_op("fn caller(p: *const i64) -> i64 { p.read() }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "raw-pointer `read`");
}

#[test]
fn test_raw_pointer_read_method_inside_unsafe_accepted() {
    let diags = lint_op("fn caller(p: *const i64) -> i64 { unsafe { p.read() } }");
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_raw_pointer_offset_method_outside_unsafe_errors() {
    let diags = lint_op("fn caller(p: *const i64) { let _ = p.offset(1i64); }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "raw-pointer `offset`");
}

#[test]
fn test_raw_pointer_write_method_outside_unsafe_errors() {
    let diags = lint_op("fn caller(p: *mut i64) { p.write(9i64); }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "raw-pointer `write`");
}

#[test]
fn test_raw_pointer_is_null_method_is_safe() {
    // `p.is_null()` is the method-form of the safe `ptr.is_null(p)` — a
    // null-bits check with no UB, so it needs no `unsafe { }`.
    let diags = lint_op("fn caller(p: *const i64) -> bool { p.is_null() }");
    assert!(diags.is_empty(), "is_null is safe; got: {diags:?}");
}

#[test]
fn test_impl_method_unsafe_fn_call_outside_unsafe_errors() {
    let diags = lint_op(
        "struct S { x: i64 }\n\
         impl S { unsafe fn raw_read(self) -> i64 { self.x } }\n\
         fn caller(s: S) -> i64 { s.raw_read() }",
    );
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    assert_unsafe_op_diag(&diags, "call to `unsafe fn S.raw_read`");
}

#[test]
fn test_impl_method_unsafe_fn_call_inside_unsafe_accepted() {
    let diags = lint_op(
        "struct S { x: i64 }\n\
         impl S { unsafe fn raw_read(self) -> i64 { self.x } }\n\
         fn caller(s: S) -> i64 { unsafe { s.raw_read() } }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

#[test]
fn test_plain_method_call_does_not_trigger() {
    let diags = lint_op(
        "struct S { x: i64 }\n\
         impl S { fn safe_read(self) -> i64 { self.x } }\n\
         fn caller(s: S) -> i64 { s.safe_read() }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// ── Slice 4: diagnostic shape ────────────────────────────────────────
//
// Every `unsafe_op_in_unsafe_fn` diagnostic carries three pieces:
// (1) a focused primary message pointing at the offending operation,
// (2) a `help` line suggesting the `unsafe { ... }` wrap + `// Safety:`
//     comment per the `undocumented_unsafe` lint, and
// (3) a `note` line distinguishing the two roles of `unsafe` — on `fn` a
//     precondition declared for callers, and on `{ ... }` a trust
//     assertion by the implementer. Conflating these is the most common
//     newcomer mistake the rule is designed to surface, so the note
//     ships in the *same* diagnostic, not as separate documentation.
//
// These tests pin the shape: presence of `help` / `note`, the actionable
// content of `help` (wrap-in-unsafe + Safety-comment guidance), and the
// presence of the two-role distinction in `note`.

fn assert_diag_has_slice4_shape(diag: &LintDiagnostic, target_substring_in_help: &str) {
    let help = diag
        .help
        .as_ref()
        .expect("slice 4: every unsafe_op_in_unsafe_fn diagnostic must carry a `help` line");
    assert!(
        help.contains("unsafe {"),
        "help should suggest wrapping in `unsafe {{ ... }}`, got: {help}"
    );
    assert!(
        help.contains("// Safety:"),
        "help should suggest adding a `// Safety:` comment, got: {help}"
    );
    assert!(
        help.contains("undocumented_unsafe"),
        "help should reference the `undocumented_unsafe` lint as the carrier, got: {help}"
    );
    assert!(
        help.contains(target_substring_in_help),
        "help should reference the offending operation ({target_substring_in_help}), got: {help}"
    );
    let note = diag
        .note
        .as_ref()
        .expect("slice 4: every unsafe_op_in_unsafe_fn diagnostic must carry a `note` line");
    // The note must surface BOTH roles of `unsafe` in the same string so a
    // first-time reader cannot conflate them: declaration-side precondition
    // for callers vs. implementer-side trust assertion on a block.
    assert!(
        note.contains("`unsafe fn`"),
        "note should reference `unsafe fn` (declaration side), got: {note}"
    );
    assert!(
        note.contains("`unsafe { ... }`"),
        "note should reference `unsafe {{ ... }}` (implementer side), got: {note}"
    );
    assert!(
        note.contains("caller"),
        "note should mention the caller's role, got: {note}"
    );
    assert!(
        note.contains("does not implicitly wrap"),
        "note should pin the `unsafe fn` body non-wrap rule, got: {note}"
    );
}

#[test]
fn test_raw_pointer_deref_diagnostic_has_slice4_shape() {
    let diags = lint_op("fn caller(p: *const i64) -> i64 { *p }");
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    let d = &diags[0];
    assert_eq!(d.level, LintLevel::Error);
    assert_eq!(d.lint_name, "unsafe_op_in_unsafe_fn");
    assert!(
        d.message.contains("raw-pointer dereference"),
        "primary message should name the operation, got: {}",
        d.message
    );
    assert!(
        d.message.contains("must be wrapped"),
        "primary message should state the rule, got: {}",
        d.message
    );
    assert_diag_has_slice4_shape(d, "pointer is valid");
}

#[test]
fn test_unsafe_fn_call_diagnostic_has_slice4_shape() {
    let diags = lint_op(
        "unsafe fn raw() {}\n\
         fn caller() { raw(); }",
    );
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    let d = &diags[0];
    assert_eq!(d.level, LintLevel::Error);
    assert_eq!(d.lint_name, "unsafe_op_in_unsafe_fn");
    assert!(
        d.message.contains("call to `unsafe fn raw`"),
        "primary message should name the callee, got: {}",
        d.message
    );
    assert!(
        d.message.contains("must be wrapped"),
        "primary message should state the rule, got: {}",
        d.message
    );
    // The help line names the specific callee so the Safety: comment author
    // knows which precondition set they are asserting.
    assert_diag_has_slice4_shape(d, "`raw`'s preconditions");
}

#[test]
fn test_unsafe_method_call_diagnostic_has_slice4_shape() {
    let diags = lint_op(
        "struct S { x: i64 }\n\
         impl S { unsafe fn raw_read(self) -> i64 { self.x } }\n\
         fn caller(s: S) -> i64 { s.raw_read() }",
    );
    assert_eq!(diags.len(), 1, "expected one error, got: {diags:?}");
    let d = &diags[0];
    assert_eq!(d.level, LintLevel::Error);
    assert_eq!(d.lint_name, "unsafe_op_in_unsafe_fn");
    assert!(
        d.message.contains("call to `unsafe fn S.raw_read`"),
        "primary message should name `Type.method`, got: {}",
        d.message
    );
    assert!(
        d.message.contains("must be wrapped"),
        "primary message should state the rule, got: {}",
        d.message
    );
    assert_diag_has_slice4_shape(d, "`S.raw_read`'s preconditions");
}

#[test]
fn test_undocumented_unsafe_diags_have_no_help_or_note() {
    // Slice 4's `help` / `note` carrier is wired through the same
    // `LintDiagnostic` struct, but it is scoped to `unsafe_op_in_unsafe_fn`.
    // The `undocumented_unsafe` lint shares the struct only as a transport;
    // it must continue to emit single-line diagnostics until its own polish
    // pass lands. This test pins that scoping decision.
    let diags = lint("fn f() {\n    unsafe { }\n}");
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].lint_name, "undocumented_unsafe");
    assert!(
        diags[0].help.is_none(),
        "undocumented_unsafe should not carry a help line yet, got: {:?}",
        diags[0].help
    );
    assert!(
        diags[0].note.is_none(),
        "undocumented_unsafe should not carry a note line yet, got: {:?}",
        diags[0].note
    );
}

#[test]
fn test_unsafe_block_wraps_multiple_ops() {
    // Inside a single `unsafe { }`, multiple unsafe ops are all accepted —
    // the context flips for the whole block.
    let diags = lint_op(
        "unsafe fn raw_a() {}\n\
         unsafe fn raw_b() {}\n\
         fn caller(p: *const i64) -> i64 {\n\
             unsafe {\n\
                 raw_a();\n\
                 raw_b();\n\
                 *p\n\
             }\n\
         }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// ── Built-in unsafe methods on prelude collections ──────────────
//
// `Vec.get_unchecked` is seeded into the unsafe-fn registry in
// `unsafe_lint::build_unsafe_fn_registry` rather than discovered from a
// user `impl ... { unsafe fn ... }` block. These tests pin the wiring
// — the diagnostic must fire at the call site when unwrapped, and stay
// silent inside `unsafe { ... }`.

#[test]
fn test_vec_get_unchecked_outside_unsafe_block_errors() {
    let diags = lint_op(
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(42);\n\
             let _ = v.get_unchecked(0);\n\
         }",
    );
    assert_eq!(
        diags.len(),
        1,
        "expected exactly one diagnostic, got: {diags:?}"
    );
    assert_eq!(diags[0].lint_name, "unsafe_op_in_unsafe_fn");
    assert!(
        diags[0].message.contains("Vec.get_unchecked"),
        "diagnostic should name `Vec.get_unchecked`, got: {}",
        diags[0].message
    );
}

#[test]
fn test_vec_get_unchecked_inside_unsafe_block_silent() {
    let diags = lint_op(
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(42);\n\
             unsafe { let _ = v.get_unchecked(0); }\n\
         }",
    );
    assert!(diags.is_empty(), "expected no diagnostics, got: {diags:?}");
}

// ── ptr.from_exposed / ptr.from_exposed_mut — line 511 slice 2 ──────
//
// Built-in module-path `unsafe fn` entries are seeded in
// `build_unsafe_fn_registry::top_level_unsafe`. The parser produces a
// `MethodCall { object: Ident("ptr"), method: "from_exposed" }` for
// `ptr.from_exposed(addr)` (the leading `ptr` is a magic module rather
// than a value), so the OpWalker's MethodCall arm checks the dotted
// name against the registry. The same shape rule covers
// `ptr.from_exposed_mut`. The safe halves of the `ptr` API
// (`addr`, `with_addr`, `expose`, …) are NOT seeded, so they must
// remain silent outside an unsafe block.

#[test]
fn test_ptr_from_exposed_outside_unsafe_block_errors() {
    let diags = lint_op("fn caller(a: usize) -> *const i64 { ptr.from_exposed(a) } fn main() {}");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("ptr.from_exposed")
                && d.lint_name == "unsafe_op_in_unsafe_fn"),
        "expected unsafe-op diagnostic naming `ptr.from_exposed`, got: {diags:?}"
    );
}

#[test]
fn test_ptr_from_exposed_mut_outside_unsafe_block_errors() {
    let diags = lint_op("fn caller(a: usize) -> *mut i64 { ptr.from_exposed_mut(a) } fn main() {}");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("ptr.from_exposed_mut")
                && d.lint_name == "unsafe_op_in_unsafe_fn"),
        "expected unsafe-op diagnostic naming `ptr.from_exposed_mut`, got: {diags:?}"
    );
}

#[test]
fn test_ptr_from_exposed_inside_unsafe_block_silent() {
    let diags = lint_op(
        "fn caller(a: usize) -> *const i64 { unsafe { ptr.from_exposed(a) } } fn main() {}",
    );
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("ptr.from_exposed")),
        "did not expect a ptr.from_exposed diagnostic, got: {diags:?}"
    );
}

#[test]
fn test_ptr_from_exposed_mut_inside_unsafe_block_silent() {
    let diags = lint_op(
        "fn caller(a: usize) -> *mut i64 { unsafe { ptr.from_exposed_mut(a) } } fn main() {}",
    );
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("ptr.from_exposed_mut")),
        "did not expect a ptr.from_exposed_mut diagnostic, got: {diags:?}"
    );
}

// ── CStr.from_ptr — the inbound raw-pointer C-string constructor ──────
//
// `CStr.from_ptr(p: *const u8)` wraps a raw, caller-owned NUL-terminated
// pointer as a borrowed `CStr` (LLVM-C FFI spike sub-q 4 — the outbound
// `char*` → owned-`String` read path). Seeded into the unsafe-fn registry
// alongside `ptr.from_exposed`; the uppercase receiver parses as a
// `Call { callee: Path(["CStr","from_ptr"]) }`, matched by the walker's
// `Call`-arm dotted-name join.

#[test]
fn test_cstr_from_ptr_outside_unsafe_block_errors() {
    let diags = lint_op("fn caller(p: *const u8) { let _c = CStr.from_ptr(p); } fn main() {}");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("CStr.from_ptr") && d.level == LintLevel::Error),
        "expected unsafe-op diagnostic naming `CStr.from_ptr`, got: {diags:?}"
    );
}

#[test]
fn test_cstr_from_ptr_inside_unsafe_block_silent() {
    let diags =
        lint_op("fn caller(p: *const u8) { let _c = unsafe { CStr.from_ptr(p) }; } fn main() {}");
    assert!(
        diags.iter().all(|d| !d.message.contains("CStr.from_ptr")),
        "did not expect a CStr.from_ptr diagnostic inside an unsafe block, got: {diags:?}"
    );
}

#[test]
fn test_ptr_addr_safe_silent() {
    // `ptr.addr` is the safe counterpart — does not require unsafe { }.
    // Regression pin: the registry must NOT have been seeded too widely.
    let diags = lint_op("fn caller(p: *const i64) -> usize { ptr.addr(p) } fn main() {}");
    assert!(
        diags.iter().all(|d| !d.message.contains("ptr.addr")),
        "ptr.addr is safe; should not produce an unsafe-op diagnostic. got: {diags:?}"
    );
}

#[test]
fn test_ptr_with_addr_safe_silent() {
    let diags = lint_op(
        "fn caller(p: *const i64, a: usize) -> *const i64 { ptr.with_addr(p, a) } fn main() {}",
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("ptr.with_addr")),
        "ptr.with_addr is safe; should not produce an unsafe-op diagnostic. got: {diags:?}"
    );
}

#[test]
fn test_ptr_expose_safe_silent() {
    let diags = lint_op("fn caller(p: *const i64) -> usize { ptr.expose(p) } fn main() {}");
    assert!(
        diags.iter().all(|d| !d.message.contains("ptr.expose")),
        "ptr.expose is safe; should not produce an unsafe-op diagnostic. got: {diags:?}"
    );
}

#[test]
fn test_ptr_container_of_outside_unsafe_block_errors() {
    let diags =
        lint_op("fn caller(fp: *const i32) -> *const i64 { ptr.container_of(fp, 0) } fn main() {}");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("ptr.container_of")
                && d.lint_name == "unsafe_op_in_unsafe_fn"),
        "expected unsafe-op diagnostic naming `ptr.container_of`, got: {diags:?}"
    );
}

#[test]
fn test_ptr_container_of_mut_outside_unsafe_block_errors() {
    let diags =
        lint_op("fn caller(fp: *mut i32) -> *mut i64 { ptr.container_of_mut(fp, 0) } fn main() {}");
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("ptr.container_of_mut")
                && d.lint_name == "unsafe_op_in_unsafe_fn"),
        "expected unsafe-op diagnostic naming `ptr.container_of_mut`, got: {diags:?}"
    );
}

#[test]
fn test_ptr_container_of_inside_unsafe_block_silent() {
    let diags = lint_op(
        "fn caller(fp: *const i32) -> *const i64 { \
             unsafe { ptr.container_of(fp, 0) } \
         } fn main() {}",
    );
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("ptr.container_of")),
        "did not expect a ptr.container_of diagnostic inside unsafe block, got: {diags:?}"
    );
}

// ── Slice 4b cross-cutting — CLI fall-through ──────────────────

#[test]
fn test_cli_allow_suppresses_undocumented_unsafe() {
    let source = "fn main() { unsafe { let _x = 0; } }";
    let prog = parse_program(source);
    let cli = karac::lints::CliLintOverrides::with_level(
        "undocumented_unsafe",
        karac::lints::LintLevel::Allow,
    );
    let diags = check_undocumented_unsafe(&prog, source, &cli);
    assert!(
        diags.is_empty(),
        "`-A undocumented_unsafe` should suppress; got: {diags:?}",
    );
}

#[test]
fn test_cli_deny_promotes_undocumented_unsafe() {
    let source = "fn main() { unsafe { let _x = 0; } }";
    let prog = parse_program(source);
    let cli = karac::lints::CliLintOverrides::with_level(
        "undocumented_unsafe",
        karac::lints::LintLevel::Deny,
    );
    let diags = check_undocumented_unsafe(&prog, source, &cli);
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    assert!(
        diags
            .iter()
            .all(|d| d.level == karac::unsafe_lint::LintLevel::Error),
        "`-D undocumented_unsafe` should promote every emission; got: {diags:?}",
    );
}

#[test]
fn test_source_allow_beats_cli_deny() {
    // Cascade precedence — source `#[allow]` on the enclosing fn wins
    // over CLI `-D undocumented_unsafe`.
    let source = "#[allow(undocumented_unsafe)]\nfn main() { unsafe { let _x = 0; } }";
    let prog = parse_program(source);
    let cli = karac::lints::CliLintOverrides::with_level(
        "undocumented_unsafe",
        karac::lints::LintLevel::Deny,
    );
    let diags = check_undocumented_unsafe(&prog, source, &cli);
    assert!(
        diags.is_empty(),
        "source `#[allow]` should beat CLI `-D`; got: {diags:?}",
    );
}

// Note: `E_UNION_READ_REQUIRES_UNSAFE` is emitted by the typechecker
// (not this lint pass) — see `tests/typechecker.rs::union_field_read_*`
// for the slice-2a coverage.
