// tests/closure_escape.rs

//! Conformance suite for the shared closure escape analysis
//! (`karac::closure_escape`) as surfaced by the `escaping_closure` check
//! lint — B-2026-08-16-13.
//!
//! The fixtures ARE the measured boundary table from the ledger row: every
//! shape `karac build` accepted on HEAD when the row was closed must stay
//! diagnostic-free, and every shape it refused must be flagged. Because the
//! lint runs codegen's OWN analysis (one predicate, no mirror), each future
//! heap-closure-environment slice that widens the supported set will move a
//! fixture from the refused table to the builds table — update the test in
//! the same commit that lands the slice, exactly as the
//! `E_ESCAPING_CLOSURE_NOT_YET` message lists are updated.
//!
//! Deliberately ungated (no `--features llvm`): the analysis is plain-AST,
//! which is the point — a non-llvm `karac check` runs the same gate.

use karac::escaping_closure_lint::check_escaping_closures;
use karac::lints::CliLintOverrides;

/// Parse, typecheck, and LOWER `src`, then run the lint with default
/// overrides (registry level: Deny) and return the diagnostic messages.
///
/// Lowering first is load-bearing, exactly as it is in the pipeline: the
/// escape analysis is not lowering-invariant (a bare `[make(..)]` literal is
/// an `ArrayLiteral` pre-lower but a Vec `PrefixCollectionLiteral` after),
/// and parity with `karac build` means analyzing the AST codegen compiles.
fn lint_messages(src: &str) -> Vec<String> {
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "fixture must parse cleanly, got {:?} for:\n{}",
        parsed.errors,
        src
    );
    let mut program = parsed.program;
    let resolved = karac::resolve(&program);
    let typed = karac::typecheck(&program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "fixture must typecheck cleanly, got {:?} for:\n{}",
        typed.errors,
        src
    );
    karac::lower(&mut program, &typed);
    let (diags, deny) = check_escaping_closures(&program, &CliLintOverrides::default());
    assert!(deny, "registry default for escaping_closure must be Deny");
    diags.into_iter().map(|d| d.message).collect()
}

fn assert_clean(src: &str) {
    let msgs = lint_messages(src);
    assert!(
        msgs.is_empty(),
        "shape BUILDS on HEAD and must stay diagnostic-free, got: {msgs:?}\nfor:\n{src}"
    );
}

fn assert_flagged(src: &str) {
    let msgs = lint_messages(src);
    assert!(
        !msgs.is_empty(),
        "shape is REFUSED by `karac build` and must be flagged at check time:\n{src}"
    );
    assert!(
        msgs.iter().all(|m| m.contains("escaping_closure")
            || m.contains("closure")
            || m.contains("E_ESCAPING_CLOSURE_NOT_YET")),
        "diagnostic must be the escaping-closure one, got: {msgs:?}"
    );
}

/// Shared prelude: a capturing-closure factory, a struct owner shape, and a
/// `Fn`-param consumer — the cast of the ledger row's measured table.
const PRELUDE: &str = r#"
struct Rule { name: String, check: Fn(ref String) -> bool }

fn min_len(n: i64) -> Fn(ref String) -> bool {
    |s| (s.len() as i64) >= n
}

fn use_it(f: Fn(ref String) -> bool) -> bool {
    let w: String = "abcdef";
    return f(w);
}

fn takes(r: Rule) -> bool {
    let w: String = "abcdef";
    return (r.check)(w);
}
"#;

fn with_prelude(main_body: &str) -> String {
    format!("{PRELUDE}\nfn main() {{\n{main_body}\n}}\n")
}

// ── The BUILDS table: every shape stays diagnostic-free ─────────────────

#[test]
fn builds_call_where_bound() {
    assert_clean(&with_prelude(
        r#"    let f = min_len(8);
    let w: String = "password1";
    println(f(w));"#,
    ));
}

#[test]
fn builds_copy_to_another_binding() {
    assert_clean(&with_prelude(
        r#"    let f = min_len(8);
    let g = f;
    let w: String = "password1";
    println(g(w));"#,
    ));
}

#[test]
fn builds_bound_value_passed_by_fn_param() {
    assert_clean(&with_prelude(
        r#"    let f = min_len(3);
    println(use_it(f));"#,
    ));
}

#[test]
fn builds_vec_fn_owner_push_and_index_call() {
    assert_clean(&with_prelude(
        r#"    let mut v: Vec[Fn(ref String) -> bool] = Vec.new();
    v.push(min_len(2));
    let w: String = "password1";
    println((v[0])(w));"#,
    ));
}

#[test]
fn builds_let_bound_struct_owner_and_field_call() {
    assert_clean(&with_prelude(
        r#"    let r = Rule { name: "min8", check: min_len(8) };
    let w: String = "password1";
    println((r.check)(w));"#,
    ));
}

#[test]
fn builds_let_bound_tuple_owner_and_index_call() {
    assert_clean(&with_prelude(
        r#"    let t = (min_len(2), 5);
    let w: String = "password1";
    println((t.0)(w));"#,
    ));
}

#[test]
fn builds_annotated_array_owner_and_index_call() {
    // The array-owner slice qualifies an `ExprKind::ArrayLiteral` RHS, which
    // is the ANNOTATED form — a bare `[..]` lowers to a Vec literal (see the
    // refused twin below). Measured against `karac build -A escaping_closure`.
    assert_clean(&with_prelude(
        r#"    let a: Array[Fn(ref String) -> bool, 1] = [min_len(2)];
    let w: String = "password1";
    println((a[0])(w));"#,
    ));
}

#[test]
fn refused_bare_vec_literal_with_heap_env_element() {
    // The bare `[min_len(2)]` lowers to a Vec `PrefixCollectionLiteral`,
    // whose heap-env element store the guard rejects (the Vec-owner slice
    // recognizes `Vec.new()` + pushes, not literals). This is also the pin
    // for WHY the lint runs post-lower: on the un-lowered AST this is an
    // `ArrayLiteral` and would be wrongly sanctioned — measured divergence,
    // caught by the build-oracle sweep when this suite landed.
    assert_flagged(&with_prelude(
        r#"    let a = [min_len(2)];
    let w: String = "password1";
    println((a[0])(w));"#,
    ));
}

#[test]
fn builds_nested_closure_capture_of_bound_value() {
    assert_clean(&with_prelude(
        r#"    let f = min_len(3);
    let h = |s: String| f(s);
    let w: String = "password1";
    println(h(w));"#,
    ));
}

#[test]
fn builds_capture_free_returned_closure_stored_anywhere() {
    // `always()` returns a NON-capturing closure (null env) — storing it in a
    // pushed struct is fine; the guard is one-sided on capturing closures.
    let src = format!(
        "{PRELUDE}
fn always() -> Fn(ref String) -> bool {{
    |s| true
}}

fn main() {{
    let mut rules: Vec[Rule] = Vec.new();
    rules.push(Rule {{ name: \"any\", check: always() }});
    let w: String = \"x\";
    let r = rules[0];
    println((r.check)(w));
}}
"
    );
    assert_clean(&src);
}

#[test]
fn builds_inline_capturing_closure_stored_anywhere() {
    // An INLINE capturing closure literal in a pushed struct builds (its env
    // lives on the pushing frame, which outlives the same-frame uses).
    assert_clean(&with_prelude(
        r#"    let n = 3i64;
    let mut rules: Vec[Rule] = Vec.new();
    rules.push(Rule { name: "inline", check: |s| (s.len() as i64) >= n });
    let w: String = "password1";
    let r = rules[0];
    println((r.check)(w));"#,
    ));
}

// ── The REFUSED table: every shape must be flagged ──────────────────────

#[test]
fn refused_fresh_call_in_pushed_struct_literal() {
    // The row's own dogfood shape: `rules.push(Rule { check: min_len(8) })`.
    assert_flagged(&with_prelude(
        r#"    let mut rules: Vec[Rule] = Vec.new();
    rules.push(Rule { name: "min8", check: min_len(8) });
    let w: String = "password1";
    let r = rules[0];
    println((r.check)(w));"#,
    ));
}

#[test]
fn refused_struct_literal_as_call_argument() {
    assert_flagged(&with_prelude(
        r#"    println(takes(Rule { name: "min2", check: min_len(2) }));"#,
    ));
}

#[test]
fn refused_direct_struct_literal_return() {
    let src = format!(
        "{PRELUDE}
fn make_rule() -> Rule {{
    return Rule {{ name: \"min2\", check: min_len(2) }};
}}

fn main() {{
    let r = make_rule();
    let w: String = \"password1\";
    println((r.check)(w));
}}
"
    );
    assert_flagged(&src);
}

#[test]
fn builds_owner_passed_to_borrows_only_callee() {
    // The row's table (measured at 0431984/cdd9bd8) listed `takes(r)` as
    // refused; the epic's by-value borrow slice has since sanctioned an owner
    // passed to a callee that only CALLS through it (`fn_param_is_borrows_only`)
    // — re-measured against `karac build -A escaping_closure` when this suite
    // landed. A mirrored lint pinned to the row's table would have been a
    // Deny-level false positive on exactly this shape, which is why the
    // predicate is shared instead of mirrored.
    assert_clean(&with_prelude(
        r#"    let r = Rule { name: "min8", check: min_len(8) };
    println(takes(r));"#,
    ));
}

#[test]
fn refused_owner_passed_to_escaping_callee() {
    // The still-refused half of the row's `takes(r)` entry: the callee
    // RETURNS its parameter, so the pass is a move-out, not a borrow.
    let src = format!(
        "{PRELUDE}
fn relay(r: Rule) -> Rule {{
    return r;
}}

fn main() {{
    let r = Rule {{ name: \"min8\", check: min_len(8) }};
    let r2 = relay(r);
    let w: String = \"password1\";
    println((r2.check)(w));
}}
"
    );
    assert_flagged(&src);
}

#[test]
fn refused_owner_pushed_on() {
    assert_flagged(&with_prelude(
        r#"    let r = Rule { name: "min8", check: min_len(8) };
    let mut rules: Vec[Rule] = Vec.new();
    rules.push(r);"#,
    ));
}

#[test]
fn refused_copy_out_of_struct_owner() {
    assert_flagged(&with_prelude(
        r#"    let r = Rule { name: "min8", check: min_len(8) };
    let g = r.check;
    let w: String = "password1";
    println(g(w));"#,
    ));
}

#[test]
fn refused_copy_out_of_tuple_owner() {
    assert_flagged(&with_prelude(
        r#"    let t = (min_len(2), 5);
    let g = t.0;
    let w: String = "password1";
    println(g(w));"#,
    ));
}

#[test]
fn refused_unbound_statement_call() {
    assert_flagged(&with_prelude(r#"    min_len(2);"#));
}

#[test]
fn refused_unbound_immediate_call() {
    assert_flagged(&with_prelude(
        r#"    let w: String = "password1";
    println(min_len(2)(w));"#,
    ));
}

#[test]
fn refused_unbound_call_as_argument() {
    assert_flagged(&with_prelude(r#"    println(use_it(min_len(3)));"#));
}

#[test]
fn refused_branch_return_of_heap_env_call() {
    let src = format!(
        "{PRELUDE}
fn pick(b: bool) -> Fn(ref String) -> bool {{
    if b {{
        return min_len(2);
    }}
    return min_len(3);
}}

fn main() {{
    let f = pick(true);
    let w: String = \"password1\";
    println(f(w));
}}
"
    );
    assert_flagged(&src);
}

// ── Enumeration parity beyond free functions ────────────────────────────

#[test]
fn refused_inside_impl_method() {
    // The compile loop validates impl methods through the SAME
    // `make_impl_method_function` synthesis the lint uses — a method whose
    // body binds a capturing closure and tail-returns the binding is refused
    // by build and must be flagged at check.
    let src = r#"
struct Fact { bias: i64 }

impl Fact {
    fn make(ref self) -> Fn(i64) -> i64 {
        let b = self.bias;
        let f = |x| x + b;
        return f;
    }
}

fn main() {
    let fac = Fact { bias: 2 };
    let f = fac.make();
    println(f(1));
}
"#;
    assert_flagged(src);
}

#[test]
fn direct_capturing_tail_closure_is_supported() {
    // Slice 1's sanctioned shape: the capturing closure literal AS the
    // function's direct tail gets a heap env and is returnable.
    assert_clean(&with_prelude(
        r#"    let f = min_len(4);
    let w: String = "password1";
    println(f(w));"#,
    ));
}

// ── Lint plumbing ───────────────────────────────────────────────────────

#[test]
fn allow_override_suppresses() {
    let mut overrides = CliLintOverrides::default();
    overrides.levels.insert(
        "escaping_closure".to_string(),
        karac::lints::LintLevel::Allow,
    );
    let src = with_prelude(
        r#"    let mut rules: Vec[Rule] = Vec.new();
    rules.push(Rule { name: "min8", check: min_len(8) });"#,
    );
    let parsed = karac::parse(&src);
    assert!(parsed.errors.is_empty());
    let (diags, deny) = check_escaping_closures(&parsed.program, &overrides);
    assert!(diags.is_empty(), "-A escaping_closure must suppress");
    assert!(!deny);
}

#[test]
fn diagnostic_carries_lint_name_and_advisory() {
    let src = with_prelude(
        r#"    let mut rules: Vec[Rule] = Vec.new();
    rules.push(Rule { name: "min8", check: min_len(8) });"#,
    );
    let parsed = karac::parse(&src);
    assert!(parsed.errors.is_empty());
    let (diags, _) = check_escaping_closures(&parsed.program, &CliLintOverrides::default());
    assert_eq!(diags.len(), 1, "one diagnostic per rejected function");
    let d = &diags[0];
    assert_eq!(d.lint_name.as_deref(), Some("escaping_closure"));
    assert!(
        d.message.contains("-A escaping_closure"),
        "must advertise the interp-only opt-out: {}",
        d.message
    );
    assert!(
        d.message
            .contains("heap-closure-environment epic B-2026-06-22-2"),
        "must carry codegen's own boundary prose: {}",
        d.message
    );
    assert!(d.fix_it.is_none(), "measured: no mechanical rewrite exists");
}
