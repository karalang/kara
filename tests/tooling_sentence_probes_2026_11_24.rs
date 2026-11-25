//! design.md § 17 Tooling contract: the probes behind
//! conformance/design/tooling/SENTENCE_MAP.md. `cases17.py` and
//! `cases17b.py` are the 2026-11-21 design-14-17 pass, moved into the repo;
//! `cases17c.py` is the 2026-11-24 sentence-map audit's. Each prints one
//! `PASS` or `FAIL` line per check and a `N pass, M fail` total.
//!
//! They build and run programs, so they need the backend and the runtime
//! archive: on the default leg, or without the archive, the test returns
//! early.

#![cfg(feature = "llvm")]

mod common;

use std::path::Path;
use std::process::Command;

/// Checks that fail today and are tracked, by the start of their name.
const OPEN: &[&str] = &[
    // PREDICTS B-2026-11-25-24: `karac run` ignores `[profile.dev]
    // runtime_debug_metadata = false` (`karac build` honors it).
    "[profile.dev] runtime_debug_metadata = false turns it off",
    // PREDICTS B-2026-11-25-25: a `shared` field's borrow conflict records kind
    // `explicit`.
    "panic record kind borrow_conflict",
];

fn probe(script: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance/design/tooling/probes");
    let out = Command::new("python3")
        .arg(dir.join(script))
        .env("K", env!("CARGO_BIN_EXE_karac"))
        .env(
            "DESIGN_MD",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/design.md"),
        )
        .output()
        .expect("python3 runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{script} crashed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let total = stdout
        .lines()
        .rev()
        .find(|l| l.ends_with(" fail"))
        .unwrap_or_else(|| panic!("{script}: no total line:\n{stdout}"));
    let passed: usize = total
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(passed > 10, "{script}: only {passed} checks passed");
    let fails: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("FAIL  "))
        .filter(|l| !OPEN.iter().any(|o| l.starts_with(o)))
        .collect();
    assert!(fails.is_empty(), "{script}:\n{}", fails.join("\n"));
}

/// Skips when this checkout cannot link a program.
fn can_link() -> bool {
    let dir = std::env::temp_dir().join(format!("karac-tooling-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("hi.kara");
    std::fs::write(&src, "fn main() {\n    println(\"hi\");\n}\n").unwrap();
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", src.to_str().unwrap(), "-o"])
        .arg(dir.join("hi"))
        .output()
        .unwrap();
    common::build_or_skip(&built).is_some()
}

#[test]
fn tooling_probes_structured_output_stream_query_fix_fmt_lints() {
    if can_link() {
        probe("cases17.py");
    }
}

#[test]
fn tooling_probes_caps_levels_queries_test_and_run_records() {
    if can_link() {
        probe("cases17b.py");
    }
}

#[test]
fn tooling_probes_sentence_map_gaps() {
    if can_link() {
        probe("cases17c.py");
    }
}
