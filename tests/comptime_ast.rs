//! Comptime AST builder + emission (substrate 3).
//!
//! Covers the quasi-quote builder `ast.expr(s)` with expression splicing
//! (the generated code runs in the surrounding scope), f-string
//! interpolation of comptime values into the quote, the `compiler.error(msg)`
//! compile-time diagnostic, and the `E_COMPTIME_MODULE_AT_RUNTIME` gate.
//! Spec: deferred.md § Comptime — AST builder API / Comptime stdlib surface.

/// Run parse → desugar → resolve → typecheck → lower → comptime-fold and
/// return the comptime diagnostics.
fn comptime_diags(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    karac::comptime_eval(&mut parsed.program, &typed)
        .iter()
        .map(|e| e.message.clone())
        .collect()
}

/// Typecheck (through desugar + resolve) and return the error messages.
fn typecheck_errors(source: &str) -> Vec<String> {
    let mut parsed = karac::parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    typed.errors.iter().map(|e| e.message.clone()).collect()
}

// ── ast.expr quasi-quote + splicing ─────────────────────────────

#[test]
fn ast_expr_splices_generated_code() {
    // The generated `x * 3` runs in the surrounding scope (references the
    // runtime binding `x`), not folded to a constant.
    let src = "
fn main() {
    let x = 10;
    let y = comptime { ast.expr(\"x * 3\") };
    println(y);
}";
    assert_eq!(karac::run_program(src), vec!["30\n"]);
}

#[test]
fn ast_expr_interpolates_comptime_value() {
    // An f-string interpolates a comptime-known value into the quoted code
    // before it is parsed: `ast.expr(f"x + {k}")` with k = 4 → `x + 4`.
    let src = "
fn main() {
    let x = 5;
    let y = comptime {
        let k = 4;
        ast.expr(f\"x + {k}\")
    };
    println(y);
}";
    assert_eq!(karac::run_program(src), vec!["9\n"]);
}

#[test]
fn ast_expr_no_comptime_errors_on_valid_quote() {
    let src = "
fn main() {
    let x = 1;
    let _y = comptime { ast.expr(\"x + 1\") };
}";
    assert!(
        comptime_diags(src).is_empty(),
        "unexpected diagnostics: {:?}",
        comptime_diags(src)
    );
}

#[test]
fn ast_expr_bad_quote_is_compile_error() {
    // A quote that doesn't parse as an expression is a comptime error.
    let src = "
fn main() {
    let _y = comptime { ast.expr(\"let let let\") };
}";
    let diags = comptime_diags(src);
    assert!(
        diags.iter().any(|d| d.contains("ast.expr")),
        "expected an ast.expr parse diagnostic; got: {diags:?}"
    );
}

// ── compiler.error: compile-time validation ─────────────────────

#[test]
fn compiler_error_is_compile_error() {
    let src = "
struct Empty {}
comptime fn require_fields(comptime T: Type) {
    if T.fields().len() == 0 { compiler.error(\"type must have at least one field\"); }
}
fn main() { comptime { require_fields(Empty) }; }";
    let diags = comptime_diags(src);
    assert!(
        diags
            .iter()
            .any(|d| d.contains("E_COMPTIME_ERROR")
                && d.contains("type must have at least one field")),
        "expected E_COMPTIME_ERROR with the user message; got: {diags:?}"
    );
}

#[test]
fn compiler_error_not_raised_when_condition_false() {
    let src = "
struct Point { x: i64, y: i64 }
comptime fn require_fields(comptime T: Type) {
    if T.fields().len() == 0 { compiler.error(\"type must have at least one field\"); }
}
fn main() { comptime { require_fields(Point) }; }";
    assert!(
        comptime_diags(src).is_empty(),
        "Point has fields — no diagnostic expected; got: {:?}",
        comptime_diags(src)
    );
}

// ── E_COMPTIME_MODULE_AT_RUNTIME ────────────────────────────────

#[test]
fn ast_module_at_runtime_is_error() {
    let src = "fn main() { let _e = ast.expr(\"1 + 1\"); }";
    let errs = typecheck_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("E_COMPTIME_MODULE_AT_RUNTIME")),
        "expected E_COMPTIME_MODULE_AT_RUNTIME; got: {errs:?}"
    );
}

#[test]
fn compiler_module_at_runtime_is_error() {
    let src = "fn main() { compiler.error(\"nope\"); }";
    let errs = typecheck_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.contains("E_COMPTIME_MODULE_AT_RUNTIME")),
        "expected E_COMPTIME_MODULE_AT_RUNTIME; got: {errs:?}"
    );
}
