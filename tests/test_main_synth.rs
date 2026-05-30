//! Slice c.2a — integration tests for `karac::test_main_synth::append_test_main`.
//!
//! The synthesizer is the bridge between `karac test`'s lowered
//! test-fn program and the codegen entry point: it appends a
//! `fn main()` that calls one specific test function. These tests
//! verify the contract by running the full
//! parse → resolve → typecheck → lower → synth → codegen → link → exec
//! pipeline against representative test-body shapes and checking the
//! resulting binary's exit code / stderr.
//!
//! Pass-asserting tests assert exit 0 with no failure marker on
//! stderr. Fail-asserting tests assert exit nonzero with a
//! `KARAC_TEST_FAILURE` marker. The slice c.1 codegen does the actual
//! assert lowering; c.2's job is just to wire the test fn into a
//! callable entry point.

#![cfg(feature = "llvm")]

mod common;

use karac::ast::{Expr, ExprKind};
use karac::test_main_synth::{append_test_main, ProviderFixture};
use karac::token::Span;

fn build_and_run_test_fn(src: &str, test_fn_name: &str) -> Option<(i32, String, String)> {
    build_and_run_test_fn_with_fixtures(src, test_fn_name, &[])
}

fn build_and_run_test_fn_with_fixtures(
    src: &str,
    test_fn_name: &str,
    fixtures: &[ProviderFixture],
) -> Option<(i32, String, String)> {
    use karac::codegen::{compile_to_object_with_options, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        let mut msg = String::from("test source failed to parse:\n");
        for e in &parsed.errors {
            msg.push_str(&format!("  {:?}\n", e));
        }
        panic!("{}", msg);
    }
    // The slice c.2 synth point — runs *before* resolve / typecheck so
    // the synthesized let bindings (one per fixture) flow through the
    // typechecker's `var_type_names` registration. With synth-after-
    // typecheck the codegen's `infer_provider_type_name` falls back to
    // a stale default for the new binding (typically the HTTP `Client`
    // built-in's name) and rejects the `with_provider` call with a
    // "no vtable found" error. The same ordering will apply to the
    // production `cmd_test` cutover in slice c.3 — each test compiles
    // as its own program, so the parse → synth → resolve → typecheck
    // → lower → codegen sequence runs per test.
    append_test_main(&mut parsed.program, test_fn_name, fixtures);

    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_synth_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_synth_{}_{}", std::process::id(), id);

    if let Err(e) =
        compile_to_object_with_options(&parsed.program, &obj_path, None, None, None, None)
    {
        panic!("codegen failed: {}", e);
    }
    link_executable(&obj_path, &exe_path).ok()?;

    let output = common::output_with_hang_watchdog(
        std::process::Command::new(&exe_path),
        std::time::Duration::from_secs(15),
    )?;

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    Some((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

#[test]
fn test_synth_passing_test_exits_zero() {
    // Synthesized main calls test_pass(), which executes assert_eq
    // successfully. Exit code 0, no failure marker on stderr.
    let r = build_and_run_test_fn(
        r#"
fn test_pass() {
    assert_eq(1 + 1, 2);
}
"#,
        "test_pass",
    );
    if let Some((exit, stdout, stderr)) = r {
        assert_eq!(
            exit, 0,
            "expected exit 0; stdout={:?} stderr={:?}",
            stdout, stderr
        );
        assert!(
            !stderr.contains("KARAC_TEST_FAILURE"),
            "stderr should not contain failure marker; got {:?}",
            stderr
        );
    }
}

#[test]
fn test_synth_failing_test_emits_marker_and_exits_nonzero() {
    // assert_eq mismatch trips the c.1 lowering: writes the JSONL
    // failure marker, exits 1. Synthesizer never sees the failure —
    // it just wires the call.
    let r = build_and_run_test_fn(
        r#"
fn test_fail() {
    assert_eq(1, 2);
}
"#,
        "test_fail",
    );
    if let Some((exit, _stdout, stderr)) = r {
        assert_ne!(exit, 0, "expected nonzero exit; stderr={:?}", stderr);
        assert!(
            stderr.contains("KARAC_TEST_FAILURE "),
            "expected failure marker on stderr; got {:?}",
            stderr
        );
        assert!(
            stderr.contains("\"left\":\"1\"") && stderr.contains("\"right\":\"2\""),
            "expected formatted left/right; got stderr={:?}",
            stderr
        );
    }
}

#[test]
fn test_synth_test_with_local_helpers() {
    // Test bodies that call helper fns defined alongside them in the
    // same module work — the synthesized main only references the test
    // fn, the helpers come along for the ride as normal Item::Function
    // items in the same Program.
    let r = build_and_run_test_fn(
        r#"
fn double(n: i64) -> i64 {
    n * 2
}

fn test_uses_helper() {
    assert_eq(double(21), 42);
}
"#,
        "test_uses_helper",
    );
    if let Some((exit, _stdout, stderr)) = r {
        assert_eq!(exit, 0, "expected exit 0; stderr={:?}", stderr);
    }
}

/// Build a `Call(Identifier(name), [])` Expr — the no-arg call shape
/// used by the fixture tests below for provider constructors like
/// `make_provider()`. Matches the AST the parser produces for the same
/// surface syntax, so codegen sees an identical shape regardless of
/// whether the call came from parsed source or synth.
fn synth_no_arg_call(name: &str) -> Expr {
    let zero = Span::default();
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Identifier(name.to_string()),
                span: zero.clone(),
            }),
            args: Vec::new(),
        },
        span: zero,
    }
}

#[test]
fn test_synth_single_fixture_wraps_test_fn() {
    // The test fn calls a resource method (`Greeting.greet()`). Without
    // a provider frame on the stack, the call would fail at the
    // runtime's `karac_provider_lookup` step. The synthesized main's
    // outermost `with_provider[Greeting](make_helper(), || test_fn())`
    // pushes a frame first, the test fn's `Greeting.greet()` resolves
    // through the vtable, the assert_eq passes.
    let src = r#"
pub trait Counter {
    fn count(mut ref self) -> i64;
}

pub effect resource Cnt: Counter;

pub struct Helper {
    val: i64,
}

impl Counter for Helper {
    fn count(mut ref self) -> i64 {
        self.val
    }
}

fn make_helper() -> Helper {
    Helper { val: 42 }
}

fn test_resolves_provider() {
    assert_eq(Cnt.count(), 42);
}
"#;
    let fixtures = vec![ProviderFixture {
        resource_path: "Cnt".to_string(),
        constructor: synth_no_arg_call("make_helper"),
    }];
    let r = build_and_run_test_fn_with_fixtures(src, "test_resolves_provider", &fixtures);
    if let Some((exit, stdout, stderr)) = r {
        assert_eq!(
            exit, 0,
            "expected exit 0; stdout={:?} stderr={:?}",
            stdout, stderr
        );
        assert!(
            !stderr.contains("KARAC_TEST_FAILURE"),
            "stderr should not contain failure marker; got {:?}",
            stderr
        );
    }
}

#[test]
fn test_synth_two_fixtures_nest_in_source_order() {
    // Both `R1.greet()` and `R2.greet()` resolve through the runtime
    // provider stack. Source-order fixtures must produce nested
    // `with_provider[R1](.., || with_provider[R2](.., || test_fn()))`
    // — if either fixture were missing or ordering were wrong, the
    // inner test fn's resource lookup would fail and the test would
    // exit non-zero.
    let src = r#"
pub trait Sayer {
    fn value(mut ref self) -> i64;
}

pub effect resource R1: Sayer;
pub effect resource R2: Sayer;

pub struct A { x: i64 }
pub struct B { y: i64 }

impl Sayer for A {
    fn value(mut ref self) -> i64 { self.x }
}

impl Sayer for B {
    fn value(mut ref self) -> i64 { self.y }
}

fn make_a() -> A { A { x: 1 } }
fn make_b() -> B { B { y: 2 } }

fn test_uses_both() {
    assert_eq(R1.value(), 1);
    assert_eq(R2.value(), 2);
}
"#;
    let fixtures = vec![
        ProviderFixture {
            resource_path: "R1".to_string(),
            constructor: synth_no_arg_call("make_a"),
        },
        ProviderFixture {
            resource_path: "R2".to_string(),
            constructor: synth_no_arg_call("make_b"),
        },
    ];
    let r = build_and_run_test_fn_with_fixtures(src, "test_uses_both", &fixtures);
    if let Some((exit, stdout, stderr)) = r {
        assert_eq!(
            exit, 0,
            "expected exit 0; stdout={:?} stderr={:?}",
            stdout, stderr
        );
    }
}

#[test]
fn test_synth_replaces_existing_main_in_program() {
    // If the source already has a `fn main() { println("user main") }`,
    // the synthesizer's filter removes it and replaces with one that
    // calls the test fn. The user's main does NOT run — its println is
    // absent from stdout.
    let r = build_and_run_test_fn(
        r#"
fn main() {
    println("user main");
}

fn test_synthesized() {
    println("synth-main ran me");
}
"#,
        "test_synthesized",
    );
    if let Some((exit, stdout, stderr)) = r {
        assert_eq!(exit, 0, "expected exit 0; stderr={:?}", stderr);
        assert!(
            stdout.contains("synth-main ran me"),
            "expected synth main to call test fn; got stdout={:?}",
            stdout
        );
        assert!(
            !stdout.contains("user main"),
            "user main should be removed; got stdout={:?}",
            stdout
        );
    }
}
