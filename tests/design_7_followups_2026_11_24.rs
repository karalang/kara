//! Review's follow-ups to the design.md §5-§7 sentence-map audit
//! (2026-11-24): disjoint closure capture at any field depth
//! (B-2026-11-24-37), a `mut ref` element of a slice pattern writes through
//! (B-2026-11-24-36), an array pattern through a reference is irrefutable,
//! and every `MutFn` call needs a mutable binding (review ruling
//! 2026-11-24, design.md §7 Function types and kinds). Each case is a
//! conformance cell, run here on the interpreter and, with the backend, on
//! `karac run`; a refused cell is checked for its diagnostic and, with a
//! `.fixed` file, for `karac fix`'s whole result.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn karac(args: &[&str], path: &Path) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn cell(dir: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance/design")
        .join(dir)
        .join(format!("{name}.kara"))
}

fn runs(dir: &str, name: &str) {
    let src = cell(dir, name);
    let want = std::fs::read_to_string(src.with_extension("out")).unwrap();
    let mut lanes = vec![vec!["run", "--interp"]];
    if cfg!(feature = "llvm") {
        lanes.push(vec!["run"]);
    }
    for lane in lanes {
        let (ok, out, err) = karac(&lane, &src);
        assert!(ok, "{name} {lane:?}: {err}");
        assert_eq!(out, want, "{name} {lane:?}: {err}");
    }
}

fn refused(dir: &str, name: &str) {
    let src = cell(dir, name);
    let (ok, out, err) = karac(&["check"], &src);
    let diag = out + &err;
    assert!(!ok, "{name} must be refused");
    for line in std::fs::read_to_string(src.with_extension("refused"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        assert!(diag.contains(line), "{name}: want {line:?}: {diag}");
    }
    let fixed = src.with_extension("fixed");
    if fixed.exists() {
        let tmp =
            std::env::temp_dir().join(format!("d7-followup-{name}-{}.kara", std::process::id()));
        std::fs::copy(&src, &tmp).unwrap();
        let _ = karac(&["fix"], &tmp);
        let got = std::fs::read_to_string(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(got, std::fs::read_to_string(&fixed).unwrap(), "{name}: fix");
        let tmp2 = std::env::temp_dir().join(format!(
            "d7-followup-{name}-fixed-{}.kara",
            std::process::id()
        ));
        std::fs::write(&tmp2, &got).unwrap();
        let (ok, _, err) = karac(&["check"], &tmp2);
        let _ = std::fs::remove_file(&tmp2);
        assert!(ok, "{name}: the fixed program is refused: {err}");
        assert!(
            !err.contains("warning"),
            "{name}: the fixed program warns: {err}"
        );
    }
}

#[test]
fn d7f_closure_captures_a_nested_field_path_apart() {
    runs("functions", "closure_captures_a_nested_field_path_apart");
    refused(
        "functions",
        "closure_nested_capture_blocks_the_same_path_refused",
    );
}

#[test]
fn d7f_slice_pattern_mut_ref_element_writes_through() {
    runs("types", "slice_pattern_mut_ref_element_writes");
    runs("types", "slice_pattern_mut_ref_element_through_mut_ref_vec");
    // Several in one pattern, each its own piece of the view (B-2026-11-25-1).
    runs("types", "slice_pattern_several_mut_ref_elements_write");
    refused(
        "types",
        "slice_pattern_mut_ref_element_over_ref_param_refused",
    );
    refused(
        "types",
        "slice_pattern_mut_ref_element_over_immutable_local_refused",
    );
}

#[test]
fn d7f_array_pattern_through_a_ref_is_irrefutable() {
    runs("types", "array_pattern_through_a_ref_is_irrefutable");
}

#[test]
fn d7f_every_mutfn_call_needs_a_mutable_binding() {
    refused("functions", "own_mutfn_param_called_directly_refused");
    refused("functions", "mutfn_taken_from_a_vec_needs_let_mut_refused");
    runs("functions", "mutfn_rebound_with_let_mut_runs");
    // A pattern binding too (review ruling 2026-11-25).
    refused("functions", "mutfn_bound_by_a_pattern_refused");
}
