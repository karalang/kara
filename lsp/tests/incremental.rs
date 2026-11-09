//! Incremental diagnostics (B-2026-11-09-33): an edit inside one function
//! body re-checks that body, and what it reports must be what a full check
//! reports. `incremental_matches_full_over_a_directory` is the sweep over
//! the corpus apps, run on demand:
//! `KARA_SWEEP_DIR=$PWD/corpus/apps cargo test --release -p kara-lsp --test incremental -- --ignored --nocapture`.

use std::time::Instant;

use karac::{CheckKind, DocumentChecker, PlaygroundDiagnostic};

/// A diagnostic as compared: every field, fixes included.
fn key(d: &PlaygroundDiagnostic) -> String {
    format!("{d:?}")
}

fn same(a: &[PlaygroundDiagnostic], b: &[PlaygroundDiagnostic]) -> Result<(), String> {
    let mut a: Vec<String> = a.iter().map(key).collect();
    let mut b: Vec<String> = b.iter().map(key).collect();
    a.sort();
    b.sort();
    if a == b {
        return Ok(());
    }
    let only_a: Vec<_> = a.iter().filter(|x| !b.contains(x)).collect();
    let only_b: Vec<_> = b.iter().filter(|x| !a.contains(x)).collect();
    Err(format!(
        "incremental only: {only_a:#?}\nfull only: {only_b:#?}"
    ))
}

/// Check `edited` after `base` both ways; the kind the checker took.
fn compare(base: &str, edited: &str) -> Result<CheckKind, String> {
    let mut c = DocumentChecker::new();
    c.full(base);
    let (inc, kind) = c.check_reporting(edited);
    same(&inc, &karac::check_source(edited))?;
    same_hints(&c, edited)?;
    // And back again, from the incremental result.
    let (back, _) = c.check_reporting(base);
    same(&back, &karac::check_source(base)).map_err(|e| format!("reverting: {e}"))?;
    same_hints(&c, base).map_err(|e| format!("reverting: {e}"))?;
    Ok(kind)
}

/// `c`'s inlay hints are a full check's of `text`.
fn same_hints(c: &DocumentChecker, text: &str) -> Result<(), String> {
    let mut fresh = DocumentChecker::new();
    fresh.full(text);
    if c.inlay_hints() == fresh.inlay_hints() {
        return Ok(());
    }
    let only_a: Vec<_> = c
        .inlay_hints()
        .iter()
        .filter(|h| !fresh.inlay_hints().contains(h))
        .collect();
    let only_b: Vec<_> = fresh
        .inlay_hints()
        .iter()
        .filter(|h| !c.inlay_hints().contains(h))
        .collect();
    Err(format!(
        "hints incremental only: {only_a:?}\nfull only: {only_b:?}"
    ))
}

const PROGRAM: &str = "\
struct P { x: i64 }

fn helper(a: i64) -> i64 {
    a + 1
}

fn noisy() {
    println(\"hi\");
}

fn uses(p: ref P) -> i64 {
    let y = helper(p.x);
    y * 2
}

fn main() {
    let p = P { x: 1 };
    println(uses(p));
    noisy();
}
";

fn edit(at_marker: &str, insert: &str) -> String {
    let i = PROGRAM.find(at_marker).expect("marker") + at_marker.len();
    format!("{}{}{}", &PROGRAM[..i], insert, &PROGRAM[i..])
}

#[test]
fn an_edit_inside_a_body_is_checked_incrementally_and_agrees() {
    // A type error, then a harmless statement, then whitespace: each inside
    // `uses`, so each is answered from the edited body alone.
    for insert in [
        "\n    let bad: i64 = \"no\";",
        "\n    let fine = 3;",
        "\n\n",
    ] {
        let edited = edit("fn uses(p: ref P) -> i64 {", insert);
        assert_eq!(
            compare(PROGRAM, &edited),
            Ok(CheckKind::Incremental),
            "{insert:?}"
        );
    }
}

#[test]
fn an_edit_that_changes_a_signature_or_what_a_body_names_checks_everything() {
    // A signature.
    let edited = PROGRAM.replace("fn helper(a: i64)", "fn helper(a: i64, b: i64)");
    assert_eq!(compare(PROGRAM, &edited), Ok(CheckKind::Full));
    // A new call: `noisy` gains a caller.
    let edited = edit("fn uses(p: ref P) -> i64 {", "\n    noisy();");
    assert_eq!(compare(PROGRAM, &edited), Ok(CheckKind::Full));
}

#[test]
fn an_edit_that_changes_a_private_functions_effects_checks_everything() {
    // `helper` was pure; printing makes it write the console, which its
    // callers infer from it. `println` is the library's, not an item of the
    // buffer, so only the effect comparison sees the change.
    let edited = edit("fn helper(a: i64) -> i64 {", "\n    println(\"side\");");
    assert_eq!(compare(PROGRAM, &edited), Ok(CheckKind::Full));
}

#[test]
fn incremental_and_full_agree_with_the_switch_off() {
    // KARAC_LSP_INCREMENTAL=0 is read per call; not set here, so this pins
    // that the unchanged text is answered from the last result.
    let mut c = DocumentChecker::new();
    let first = c.full(PROGRAM);
    let (again, kind) = c.check_reporting(PROGRAM);
    assert_eq!(kind, CheckKind::Unchanged);
    assert_eq!(same(&first, &again), Ok(()));
}

/// Statement starts inside bodies, spread through the file.
fn edit_points(src: &str, n: usize) -> Vec<usize> {
    let all: Vec<usize> = src
        .match_indices("\n    let ")
        .map(|(i, _)| i + 1)
        .collect();
    if all.is_empty() {
        return Vec::new();
    }
    (0..n)
        .map(|k| all[(all.len() * (2 * k + 1)) / (2 * n)])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[test]
#[ignore]
fn incremental_matches_full_over_a_directory() {
    let dir = std::env::var("KARA_SWEEP_DIR").expect("KARA_SWEEP_DIR");
    let filter = std::env::var("KARA_SWEEP_FILTER").unwrap_or_default();
    let mut files: Vec<_> = walk(std::path::Path::new(&dir))
        .into_iter()
        .filter(|p| p.ends_with("source.kara") || p.extension().is_some_and(|e| e == "kara"))
        .filter(|p| !p.to_string_lossy().ends_with(".orig.kara"))
        .filter(|p| !p.to_string_lossy().ends_with(".fixed.kara"))
        .filter(|p| p.to_string_lossy().contains(&filter))
        .collect();
    files.sort();
    // A path that matches nothing must not read as a pass.
    assert!(!files.is_empty(), "no .kara files under {dir}");
    let (mut edits, mut inc, mut failures) = (0, 0, Vec::new());
    let (mut t_inc, mut t_full) = (0.0, 0.0);
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut c = DocumentChecker::new();
        c.full(&src);
        for (k, at) in edit_points(&src, 3).into_iter().enumerate() {
            let insert = [
                "let probe_e: i64 = \"x\";\n    ",
                "let probe_u = 1;\n    ",
                "\n",
            ][k % 3];
            let edited = format!("{}{}{}", &src[..at], insert, &src[at..]);
            let t = Instant::now();
            let (got, kind) = c.check_reporting(&edited);
            t_inc += t.elapsed().as_secs_f64();
            let t = Instant::now();
            let mut fresh = DocumentChecker::new();
            let want = fresh.full(&edited);
            t_full += t.elapsed().as_secs_f64();
            edits += 1;
            if kind == CheckKind::Incremental {
                inc += 1;
            }
            let hints = if c.inlay_hints() == fresh.inlay_hints() {
                Ok(())
            } else {
                Err("inlay hints differ".to_string())
            };
            if let Err(e) = same(&got, &want).and(hints) {
                failures.push(format!(
                    "{} @{at} {insert:?} ({kind:?}): {e}",
                    path.display()
                ));
            }
            // Back to the original, from the edited result.
            let (back, _) = c.check_reporting(&src);
            if let Err(e) = same(&back, &karac::check_source(&src)).and(same_hints(&c, &src)) {
                failures.push(format!("{} @{at} revert: {e}", path.display()));
            }
        }
    }
    println!(
        "SWEEP files={} edits={edits} incremental={inc} failures={} incremental_s={t_inc:.1} full_s={t_full:.1}",
        files.len(),
        failures.len()
    );
    for f in &failures {
        println!("FAIL {}", &f[..f.len().min(3000)]);
    }
    assert!(failures.is_empty());
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
