// tests/simd_report.rs
//
// SIMD scalarization analysis + `#[require_simd]` guarantee
// (phase-7-codegen.md line 308, slice 5a). These exercise the pure
// detection core (`analyze_program` / `require_simd_errors`) over a
// real parse → resolve → typecheck pipeline — no LLVM backend needed,
// since the classification is a target model, not an instruction-select
// query.

use karac::simd_report::{analyze_program, require_simd_errors, ScalarReason, SimdTier};
use karac::{parse, resolve, typecheck};

/// Run the front-end to a `TypeCheckResult` and return the per-op SIMD
/// findings for the whole program.
fn findings(source: &str) -> Vec<karac::simd_report::SimdFinding> {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    let typed = typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "type errors: {:?}",
        typed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    analyze_program(&parsed.program, Some(&typed))
}

fn require_errors(source: &str) -> Vec<karac::simd_report::SimdFinding> {
    require_simd_errors(&findings(source))
}

#[test]
fn require_simd_rejects_non_power_of_two_lanes() {
    // `Vector[i32, 3]` — 3 lanes is not a power of two, so every op on it
    // (construction + the add) scalarizes. `#[require_simd]` must reject.
    let src = r#"
        #[require_simd]
        fn add3(a: Vector[i32, 3], b: Vector[i32, 3]) -> Vector[i32, 3] {
            a + b
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    assert!(
        !errs.is_empty(),
        "expected a require_simd rejection for the Vector[i32, 3] add"
    );
    assert!(
        errs.iter()
            .all(|e| e.reason == Some(ScalarReason::NonPowerOfTwoLanes)),
        "non-pow2 lanes should be the reported reason, got {:?}",
        errs.iter().map(|e| e.reason).collect::<Vec<_>>()
    );
    assert!(
        errs.iter().all(|e| e.func_name == "add3"),
        "the enclosing function name should be attributed"
    );
    // The message + help should name the type and the power-of-two fix.
    let msg = errs[0].message();
    assert!(
        msg.contains("Vector[i32, 3]"),
        "message names the type: {msg}"
    );
    assert!(
        msg.contains("scalar"),
        "message mentions scalarization: {msg}"
    );
    assert!(
        errs[0].help().contains("Vector[i32, 4]"),
        "help suggests the next power-of-two lane count: {}",
        errs[0].help()
    );
}

#[test]
fn require_simd_rejects_unsupported_128bit_element() {
    // 128-bit integer lanes have no SIMD ALU on any target → Scalar even
    // with a power-of-two lane count.
    let src = r#"
        #[require_simd]
        fn wide(a: Vector[i128, 4], b: Vector[i128, 4]) -> Vector[i128, 4] {
            a + b
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    assert!(
        !errs.is_empty(),
        "expected a require_simd rejection for the Vector[i128, 4] add"
    );
    assert!(
        errs.iter()
            .any(|e| e.reason == Some(ScalarReason::UnsupportedElement)),
        "128-bit element should report UnsupportedElement, got {:?}",
        errs.iter().map(|e| e.reason).collect::<Vec<_>>()
    );
}

#[test]
fn require_simd_accepts_native_power_of_two() {
    // `Vector[i32, 4]` = 128 bits = one register → Native; no rejection.
    let src = r#"
        #[require_simd]
        fn add4(a: Vector[i32, 4], b: Vector[i32, 4]) -> Vector[i32, 4] {
            a + b
        }
        fn main() {}
    "#;
    assert!(
        require_errors(src).is_empty(),
        "a native power-of-two vector op must not be rejected under #[require_simd]"
    );
}

#[test]
fn require_simd_accepts_wide_power_of_two() {
    // `Vector[i32, 8]` = 256 bits → Wide (two 128-bit ops) — still vectorised,
    // never a scalar loop, so `#[require_simd]` accepts it.
    let src = r#"
        #[require_simd]
        fn add8(a: Vector[i32, 8], b: Vector[i32, 8]) -> Vector[i32, 8] {
            a + b
        }
        fn main() {}
    "#;
    assert!(
        require_errors(src).is_empty(),
        "a wide (multi-op) power-of-two vector op must not be rejected"
    );
    // But the finding itself should classify Wide.
    let fs = findings(src);
    assert!(
        fs.iter().any(|f| f.tier == SimdTier::Wide),
        "the add8 op should classify Wide, got {:?}",
        fs.iter()
            .map(|f| (f.op_desc.clone(), f.tier))
            .collect::<Vec<_>>()
    );
}

#[test]
fn without_require_simd_scalar_ops_are_not_errors() {
    // Same scalarizing op, but no attribute — the analysis still records
    // Scalar findings (for `--simd-report`), but they are NOT build errors.
    let src = r#"
        fn add3(a: Vector[i32, 3], b: Vector[i32, 3]) -> Vector[i32, 3] {
            a + b
        }
        fn main() {}
    "#;
    assert!(
        require_errors(src).is_empty(),
        "without #[require_simd], scalar fallback is silent, not an error"
    );
    let fs = findings(src);
    assert!(
        fs.iter()
            .any(|f| f.tier == SimdTier::Scalar && !f.require_simd),
        "the analysis should still surface the Scalar finding for the report"
    );
}

#[test]
fn require_simd_on_impl_method_is_attributed() {
    // The guarantee applies to impl methods too, attributed as `Type.method`.
    let src = r#"
        struct V3 { x: i32 }
        impl V3 {
            #[require_simd]
            fn scaled(a: Vector[f32, 3], b: Vector[f32, 3]) -> Vector[f32, 3] {
                a * b
            }
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    assert!(!errs.is_empty(), "Vector[f32, 3] mul must be rejected");
    assert!(
        errs.iter().all(|e| e.func_name == "V3.scaled"),
        "impl method should be attributed as `V3.scaled`, got {:?}",
        errs.iter().map(|e| e.func_name.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn comparison_op_classified_by_operand_type() {
    // A comparison produces a `Vector[bool, N]` mask, but the *operands* are
    // `Vector[i32, 3]` — the op should be classified by the operand type
    // (Scalar), not by the bool mask. The mask type is internal and cannot be
    // written as a source annotation, so it is inferred and discarded here.
    let src = r#"
        #[require_simd]
        fn lt(a: Vector[i32, 3], b: Vector[i32, 3]) {
            let _ = a < b;
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    let cmp = errs
        .iter()
        .find(|e| e.op_desc.contains("comparison"))
        .unwrap_or_else(|| {
            panic!(
                "the comparison op should be flagged, got {:?}",
                errs.iter().map(|e| e.op_desc.clone()).collect::<Vec<_>>()
            )
        });
    // The element must be the operand element `i32`, NOT the `bool` mask the
    // comparison produces (the right-operand recovery, not the node result).
    assert_eq!(
        cmp.element, "i32",
        "comparison should be classified by the i32 operand, not the bool mask"
    );
}

#[test]
fn require_simd_rejects_scalar_reduction() {
    // A `reduce_sum` on a non-pow2 vector scalarizes — and the receiver's
    // `(T, N)` is recovered from the typechecker side-table (the method node
    // overwrites its own span with the scalar `i32` result type).
    let src = r#"
        #[require_simd]
        fn total(a: Vector[i32, 3]) -> i32 {
            a.reduce_sum()
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    let red = errs
        .iter()
        .find(|e| e.op_desc.contains("reduce_sum"))
        .unwrap_or_else(|| {
            panic!(
                "the reduce_sum op should be flagged, got {:?}",
                errs.iter().map(|e| e.op_desc.clone()).collect::<Vec<_>>()
            )
        });
    assert_eq!(red.element, "i32");
    assert_eq!(red.lanes, 3);
}

#[test]
fn require_simd_rejects_scalar_dot() {
    let src = r#"
        #[require_simd]
        fn d(a: Vector[i32, 3], b: Vector[i32, 3]) -> i32 {
            a.dot(b)
        }
        fn main() {}
    "#;
    let errs = require_errors(src);
    assert!(
        errs.iter()
            .any(|e| e.op_desc.contains("dot") && e.element == "i32"),
        "the dot op should be flagged on its i32 receiver, got {:?}",
        errs.iter()
            .map(|e| (e.op_desc.clone(), e.element.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn require_simd_accepts_native_reduction() {
    // `reduce_sum` on a native (pow2, fits-register) vector is not a Scalar op.
    let src = r#"
        #[require_simd]
        fn total(a: Vector[f32, 4]) -> f32 {
            a.reduce_sum()
        }
        fn main() {}
    "#;
    assert!(
        require_errors(src).is_empty(),
        "reduce_sum on a native Vector[f32, 4] must not be rejected"
    );
}

#[test]
fn analyze_returns_empty_without_typecheck() {
    let parsed = parse("fn main() {}");
    assert!(analyze_program(&parsed.program, None).is_empty());
}

// ── End-to-end CLI enforcement ──────────────────────────────────
//
// `karac check` is the always-available (non-LLVM) front-end surface, and a
// `#[require_simd]` violation means the program won't build — so the check
// path enforces the guarantee. These exercise the full CLI glue
// (`simd_check` → `total_errors` → `print_text_diagnostics`), not just the
// analysis core.

fn karac_check(source: &str, name: &str) -> std::process::Output {
    karac_check_flagged(source, name, &[])
}

fn karac_check_flagged(source: &str, name: &str, flags: &[&str]) -> std::process::Output {
    let path = std::env::temp_dir().join(format!("karac_simd_{name}_{}.kara", std::process::id()));
    std::fs::write(&path, source).expect("write temp .kara");
    let bin = std::env::var("CARGO_BIN_EXE_karac")
        .expect("CARGO_BIN_EXE_karac not set — run via `cargo test`");
    let mut args = vec!["check".to_string(), path.to_str().unwrap().to_string()];
    args.extend(flags.iter().map(|s| s.to_string()));
    let out = std::process::Command::new(&bin)
        .args(&args)
        .output()
        .expect("failed to run karac check");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn karac_check_rejects_require_simd_scalarization() {
    let src = "#[require_simd]\n\
        fn add3(a: Vector[i32, 3], b: Vector[i32, 3]) -> Vector[i32, 3] { a + b }\n\
        fn main() {}\n";
    let out = karac_check(src, "reject");
    assert!(
        !out.status.success(),
        "karac check must fail on a #[require_simd] scalarization"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("E_REQUIRE_SIMD"),
        "stderr should carry the E_REQUIRE_SIMD diagnostic:\n{stderr}"
    );
    assert!(
        stderr.contains("add3"),
        "the diagnostic should name the offending function:\n{stderr}"
    );
}

#[test]
fn karac_check_accepts_native_require_simd() {
    let src = "#[require_simd]\n\
        fn add4(a: Vector[i32, 4], b: Vector[i32, 4]) -> Vector[i32, 4] { a + b }\n\
        fn main() {}\n";
    let out = karac_check(src, "accept");
    assert!(
        out.status.success(),
        "karac check should pass for a native power-of-two vector op:\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── --simd-report=verbose (slice 5b) ────────────────────────────

// A program with a native op and a scalar op, no `#[require_simd]` — so it
// type-checks clean and the report (not an error) is what we assert on.
const MIXED_TIERS_SRC: &str = "\
    fn native4(a: Vector[i32, 4], b: Vector[i32, 4]) -> Vector[i32, 4] { a + b }\n\
    fn scalar3(a: Vector[i32, 3], b: Vector[i32, 3]) -> Vector[i32, 3] { a + b }\n\
    fn main() {}\n";

#[test]
fn karac_check_simd_report_lists_tiers() {
    let out = karac_check_flagged(MIXED_TIERS_SRC, "report", &["--simd-report=verbose"]);
    assert!(
        out.status.success(),
        "no #[require_simd], so check should pass:\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SIMD lowering report"),
        "report header should appear on stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("fn native4") && stdout.contains("native"),
        "the native op should be listed:\n{stdout}"
    );
    assert!(
        stdout.contains("fn scalar3")
            && stdout.contains("Vector[i32, 3]")
            && stdout.contains("SCALAR"),
        "the scalar op should be listed with its tier:\n{stdout}"
    );
}

#[test]
fn karac_check_without_flag_prints_no_simd_report() {
    let out = karac_check(MIXED_TIERS_SRC, "noreport");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("SIMD lowering report"),
        "no report should print without --simd-report:\n{stdout}"
    );
}

#[test]
fn karac_check_simd_report_bare_flag_alias() {
    // bare `--simd-report` is accepted as an alias for `--simd-report=verbose`.
    let out = karac_check_flagged(MIXED_TIERS_SRC, "barereport", &["--simd-report"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("SIMD lowering report"),
        "bare --simd-report should emit the report"
    );
}

#[test]
fn karac_check_simd_report_rejects_unknown_level() {
    let out = karac_check_flagged(MIXED_TIERS_SRC, "badlevel", &["--simd-report=loud"]);
    assert!(!out.status.success(), "unknown level should be rejected");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown --simd-report level"),
        "should name the bad level"
    );
}
