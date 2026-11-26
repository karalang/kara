//! `lossless_cast` on an operand of a binary operation or compound
//! assignment that is a `ref` to the narrower integer: `total += x as i64`
//! and `total + x as i64`, with `x` the `ref i32` that `v.iter()` yields
//! and `total: i64`. The cast's source is read through the reference, as
//! `lint_unneeded_cast` already did for a binding's initializer, so the
//! widening is linted and the fix drops it (B-2026-11-25-19).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-operand-cast-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

/// `karac fix` then `karac check` on `src`: the fixed text, and whether it
/// checks clean.
fn fix_then_check(src: &str) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-operand-cast-fix-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&f)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&f).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const SRC: &str = "fn main() { let v: Vec[i32] = [1, 2, 40]; let mut total: i64 = 0; \
     let mut t2: i64 = 100; for y in v.iter() { let a = total + y as i64; total += y as i64; \
     t2 = t2 + y as i64; println(a); } println(f\"{total} {t2}\"); }\n";

#[test]
fn a_widening_cast_of_a_ref_operand_is_linted() {
    let (ok, out) = check(SRC);
    assert!(ok, "{out}");
    assert_eq!(out.matches("warning[lossless_cast]").count(), 3, "{out}");
    assert!(
        out.contains("'i32' widens to 'i64' implicitly here"),
        "{out}"
    );
}

#[test]
fn the_fix_drops_each_cast_and_the_program_prints_the_same() {
    let want = "1\n3\n43\n43 143\n";
    runs_everywhere(SRC, want);
    let (fixed, ok, out) = fix_then_check(SRC);
    assert!(ok, "{fixed}\n{out}");
    assert!(!fixed.contains(" as i64"), "{fixed}");
    assert!(
        fixed.contains("let a = total + y; total += y; t2 = t2 + y;"),
        "{fixed}"
    );
    runs_everywhere(&fixed, want);
}

#[test]
fn a_cast_that_changes_sign_in_a_comparison_stays_unlinted() {
    let (ok, out) = check(
        "fn main() { let v: Vec[u32] = [1, 2]; let lim: i64 = 2; let mut n = 0; \
         for y in v.iter() { if (y as i64) < lim { n += 1; } } println(n); }\n",
    );
    assert!(ok, "{out}");
    assert!(!out.contains("lossless_cast"), "{out}");
}
