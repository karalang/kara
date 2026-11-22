//! B-2026-11-22-20: the documentation's Kāra blocks are checked, and run,
//! on every default `cargo test --all`, not only in CI's separate jobs.
//!
//! `scripts/design-conformance.py` sweeps docs/design.md and
//! `scripts/library-conformance.py` the library docs, README.md, the
//! examples and the book (docs/design-conformance.md says how). Each block
//! must check; a whole program must also run and print what the doc shows
//! after it; a block the doc says is rejected must be rejected at the line
//! it marks, or with the code its fence names (```kara,error=E0205); a
//! block that is not a program says so (```kara,fragment); a block that
//! builds on declarations it does not show names them (```kara,uses=point,
//! from a `name=point` block or docs/doc-context.kara). The baselines hold
//! only findings with a ledger row, so any new finding fails here. `tests/book_snippets.rs` reads the
//! same fence words for the book in-process.

use std::path::Path;
use std::process::Command;

fn sweep(script: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("python3")
        .arg(root.join("scripts").join(script))
        .arg("--check")
        .arg("--karac")
        .arg(env!("CARGO_BIN_EXE_karac"))
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("python3 is needed to run {script}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // A sweep that judged nothing is not a pass.
    assert!(
        stdout.contains("conformance:"),
        "{script} printed no summary:\n{stdout}\n{stderr}"
    );
    assert!(
        out.status.success(),
        "{script} --check found blocks the compiler does not honour (fix the \
         doc, mark the block, or fix the compiler):\n{stdout}\n{stderr}"
    );
}

#[test]
fn docs_conformance_design_md() {
    sweep("design-conformance.py");
}

#[test]
fn docs_conformance_library_docs_book_and_examples() {
    sweep("library-conformance.py");
}
