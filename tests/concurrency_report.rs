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

/// B-2026-08-01-33 (reporting half) — a loop CONSIDERED for disjoint-write
/// fan-out and declined must be visible in the human report.
///
/// The A/B is the one from the ledger entry: two programs differing ONLY in the
/// `shared` keyword on the struct declaration, bodies byte-identical. The
/// control proves `disjoint_writes`; the subject is declined by the
/// cross-task-safety gate (a `shared struct`'s refcount is not atomic). Before
/// this, the subject's report was byte-identical to a program that had no
/// candidate loop at all — the decline was invisible in the one tool a user
/// would reach for to ask exactly this question, and nothing hinted that
/// `karac query concurrency` held the answer.
///
/// The reason itself deliberately stays in `query concurrency`: one aggregate
/// line here keeps the opportunities the report exists to show from being
/// buried under per-loop decline records.
#[test]
fn test_auto_par_disabled_is_reflected_in_fanned_out() {
    // B-2026-08-05-13 — `KARAC_AUTO_PAR=0` disables every auto-par lowering,
    // so codegen emits no dispatch. The query used to report `fanned_out: true`
    // anyway, because it ran the proof and the cost model but not the env gate
    // codegen checks first — describing a binary that does not exist.
    //
    // Both callers now read `par_cost::auto_par_enabled()`, so they cannot
    // disagree. This asserts BOTH directions: without the var the loop still
    // reports fanned out, so the test cannot pass by reporting `false`
    // unconditionally.
    const SRC: &str = "\n\
fn work(k: i64) -> i64 {\n\
    let mut acc: i64 = 0;\n\
    let mut t: i64 = 0;\n\
    while t < 500 { acc = acc + k * 7 + t; t = t + 1; }\n\
    acc\n\
}\n\
fn fill(out: mut ref Vec[i64]) {\n\
    for i in 0..4096 { out[i] = work(i); }\n\
}\n\
fn main() {\n\
    let mut a: Vec[i64] = Vec.filled(4096, 0);\n\
    fill(mut a);\n\
    println(f\"{a[10]}\");\n\
}\n";

    let dir = std::env::temp_dir().join("karac_auto_par_disabled_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ap.kara");
    std::fs::write(&path, SRC).unwrap();

    let run = |disabled: bool| -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
        cmd.args(["query", "concurrency", path.to_str().unwrap()]);
        if disabled {
            cmd.env("KARAC_AUTO_PAR", "0");
        } else {
            cmd.env_remove("KARAC_AUTO_PAR");
        }
        let out = cmd.output().expect("karac query concurrency");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let on = run(false);
    assert!(
        on.contains("\"fanned_out\":true"),
        "with auto-par ON this loop must report fanned out, else the disabled \
         case below proves nothing; got:\n{on}"
    );

    let off = run(true);
    assert!(
        !off.contains("\"fanned_out\":true"),
        "with KARAC_AUTO_PAR=0 nothing is dispatched, so no loop may report \
         fanned_out: true; got:\n{off}"
    );
    assert!(
        off.contains("declined_auto_par_disabled"),
        "the disabled case must name the gate that declined it, not report a \
         cost-model verdict it never reached; got:\n{off}"
    );
}

#[test]
fn test_declined_disjoint_write_loop_is_visible_in_the_report() {
    const BODY: &str = "\n\
fn main() {\n\
    let n = 400;\n\
    let mut ps: Vec[P] = Vec.new();\n\
    for i in 0..n { ps.push(P { v: i }); }\n\
    let mut out: Vec[i64] = Vec.new();\n\
    for i in 0..n { out.push(0); }\n\
    for j in 0..n {\n\
        let q = ps[j];\n\
        let mut acc = 0;\n\
        for k in 0..500 { acc = acc + q.v * 2; }\n\
        out[j] = acc;\n\
    }\n\
    println(f\"{out[3]}\");\n\
}\n";

    let dir = std::env::temp_dir().join("karac_declined_report_test");
    std::fs::create_dir_all(&dir).unwrap();
    let ctl = dir.join("ctl.kara");
    let sub = dir.join("sub.kara");
    std::fs::write(&ctl, format!("struct P {{ v: i64 }}{BODY}")).unwrap();
    std::fs::write(&sub, format!("shared struct P {{ v: i64 }}{BODY}")).unwrap();

    let run = |path: &std::path::Path| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["check", path.to_str().unwrap(), "--concurrency-report"])
            .output()
            .expect("karac check");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let ctl_out = run(&ctl);
    let sub_out = run(&sub);

    // Control: the loop is proven, so it is listed and NO decline footer fires.
    assert!(
        ctl_out.contains("disjoint_writes {"),
        "control must prove the disjoint-write loop; got:\n{ctl_out}"
    );
    assert!(
        !ctl_out.contains("declined"),
        "the footer must NOT fire when every candidate was proven; got:\n{ctl_out}"
    );

    // Subject: declined, and the report says so and points at the tool that
    // carries the reason.
    assert!(
        !sub_out.contains("disjoint_writes {"),
        "the shared subject must not prove the loop; got:\n{sub_out}"
    );
    assert!(
        sub_out.contains("considered for disjoint-write fan-out and declined"),
        "the decline must be visible in the report; got:\n{sub_out}"
    );
    assert!(
        sub_out.contains("karac query concurrency"),
        "the footer must point at the tool carrying the reason; got:\n{sub_out}"
    );

    // The whole point: the two reports must now DIFFER.
    assert_ne!(
        ctl_out, sub_out,
        "reports for a proven and a declined loop must not be identical"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
