//! Slice D — `karac build --concurrency-report` human-readable renderer
//! (drafted 2026-05-08).
//!
//! Tests:
//! - `test_concurrency_report_renders_parallax_lite_workload`: snapshot pin
//!   on the concatenated `examples/parallax_lite/src/{resources,workload}.kara`
//!   project — the same source the canonical Parallax-lite suite uses
//!   (`tests/parallax_lite.rs`). Verifies the demo storyboard's text shape
//!   end-to-end against the locked golden file at
//!   `tests/snapshots/concurrency_report_parallax_lite.txt`.
//! - `test_build_without_concurrency_report_flag_prints_nothing`: opt-in
//!   regression — invokes `karac check` (the always-available analysis
//!   surface; `karac build` requires `--features llvm`) on the parallax-lite
//!   source and asserts stdout contains no concurrency-report header. The
//!   `--concurrency-report` flag is opt-in for both the build and check
//!   paths and must not perturb existing output when absent.
//!
//! The unit tests for the renderer's empty-case + trivial-group branches
//! live inside `src/concurrency_report.rs`'s `#[cfg(test)]` block, since
//! they construct `ConcurrencyAnalysis` and `EffectCheckResult` directly
//! and don't need the binary surface.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Concatenate the parallax-lite workload source the same way the canonical
/// suite does — resources + workload, with the cross-module `import` line
/// dropped (everything is in one `Program` after concat).
fn workload_source() -> String {
    let root = workspace_root();
    let resources = std::fs::read_to_string(root.join("examples/parallax_lite/src/resources.kara"))
        .expect("resources.kara missing");
    let workload = std::fs::read_to_string(root.join("examples/parallax_lite/src/workload.kara"))
        .expect("workload.kara missing");
    let workload_no_import: String = workload
        .lines()
        .filter(|l| !l.trim_start().starts_with("import "))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{resources}\n{workload_no_import}\n")
}

#[test]
fn test_concurrency_report_renders_parallax_lite_workload() {
    let src = workload_source();
    let mut parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors on parallax-lite workload: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let analysis = karac::concurrency_analyze(&parsed.program, &effects);

    let actual =
        karac::concurrency_report::render_concurrency_report(&analysis, &effects, &parsed.program);

    let snapshot_path =
        workspace_root().join("tests/snapshots/concurrency_report_parallax_lite.txt");
    let expected = std::fs::read_to_string(&snapshot_path).expect(
        "tests/snapshots/concurrency_report_parallax_lite.txt missing — \
         run the test once to print the actual output, then save it.",
    );

    if actual != expected {
        panic!(
            "concurrency report snapshot mismatch.\n\nExpected ({} bytes):\n{}\n\
             Actual ({} bytes):\n{}\n\
             To accept the new output, overwrite {}",
            expected.len(),
            expected,
            actual.len(),
            actual,
            snapshot_path.display()
        );
    }
}

#[test]
fn test_build_without_concurrency_report_flag_prints_nothing() {
    // Use `karac check` as the always-available surface — `karac build`
    // requires `--features llvm` to actually run, so we go through the
    // shared `--concurrency-report` plumbing on `cmd_check` instead. The
    // flag wiring is symmetric (Slice D sub-step h), so this regression
    // covers `cmd_build` by construction.
    let bin = std::env::var("CARGO_BIN_EXE_karac")
        .expect("CARGO_BIN_EXE_karac not set — run via `cargo test`");
    let out = Command::new(&bin)
        .args(["check", "examples/parallax_lite/src/workload.kara"])
        .current_dir(workspace_root())
        .output()
        .expect("failed to run karac check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("parallel_group {"),
        "no `parallel_group` block should be printed without --concurrency-report; \
         stdout was:\n{stdout}"
    );
    assert!(
        !stdout.contains("function process_request"),
        "no concurrency-report function header should appear without --concurrency-report; \
         stdout was:\n{stdout}"
    );
}
