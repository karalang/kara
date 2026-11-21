//! B-2026-11-22-20: every diagnostic code the compiler can emit has a
//! `karac explain` entry.
//!
//! `tests/diagnostics_coverage.rs` holds the other direction (every code the
//! catalogue lists has a program that produces it). This one reads the
//! compiler's source for the codes it writes: every string literal shaped like
//! a code (`"E0200"`, `"W0244"`, `"E_MODULE_LET"`, `"W_DEPENDENCY_YANKED"`)
//! outside a `#[cfg(test)]` module must be one `karac explain` answers, so a
//! new diagnostic cannot ship with a code nobody can look up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The code-shaped literals of `text` before its first module-level
/// `#[cfg(test)]`, each with its line.
fn code_literals(text: &str) -> Vec<(String, usize)> {
    let shape =
        regex::Regex::new(r#""(E[0-9]{4}|W[0-9]{4}|E_[A-Z0-9_]{3,}|W_[A-Z0-9_]{3,})""#).unwrap();
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.starts_with("#[cfg(test)]") {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        for m in shape.captures_iter(line) {
            // `"E_GPU_"`: a family's prefix, which explain.rs matches codes
            // against, not a code.
            if !m[1].ends_with('_') {
                out.push((m[1].to_string(), n + 1));
            }
        }
    }
    out
}

#[test]
fn explain_coverage_every_emitted_code_is_catalogued() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (code, line) in code_literals(&text) {
            seen.entry(code).or_insert_with(|| {
                format!(
                    "{}:{line}",
                    f.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .display()
                )
            });
        }
    }
    // A sweep that finds almost nothing has stopped reading the source.
    assert!(
        seen.len() > 300,
        "only {} code literals found under src/: the scan is broken",
        seen.len()
    );
    let missing: Vec<String> = seen
        .iter()
        .filter(|(code, _)| !karac::cli::explain::is_catalogued_code(code))
        .map(|(code, at)| format!("{code} (first at {at})"))
        .collect();
    assert!(
        missing.is_empty(),
        "{} code(s) the compiler writes have no `karac explain` entry; add each \
         to CODE_TABLE and CODE_PROSE in src/cli/explain.rs:\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}
