//! A `let` of a `match`, `if` or block whose every result is a reference
//! binds a reference (core semantics §4.6, B-2026-10-28-25): `let r = match
//! m { Some(ref r) => r, None => return -1 }` over an owned local moves
//! nothing out of `m`, so a loop matches it again, for a payload with no
//! `.clone()`. The program is the core pin
//! `corpus/core/ok_match_result_is_a_ref`, with its expected output.
//!
//! With `--features llvm` each program also runs under AddressSanitizer
//! (`karac __mir-llvm-asan`) and must print the same, with nothing
//! reported.

#[cfg(feature = "llvm")]
mod common;

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked, and so
/// does the program compiled with AddressSanitizer.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
    #[cfg(feature = "llvm")]
    asan::runs_clean(src, want);
}

#[cfg(feature = "llvm")]
mod asan {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Whether `cc -fsanitize=address` links and runs here.
    fn available(dir: &std::path::Path) -> bool {
        if std::env::var_os("KARAC_SKIP_ASAN_TESTS").is_some() {
            return false;
        }
        let (c, exe) = (dir.join("probe.c"), dir.join("probe"));
        std::fs::write(&c, "int main(void){return 0;}\n").expect("write probe");
        Command::new("cc")
            .arg("-fsanitize=address")
            .arg(&c)
            .arg("-o")
            .arg(&exe)
            .status()
            .is_ok_and(|s| s.success())
            && Command::new(&exe).status().is_ok_and(|s| s.success())
    }

    pub fn runs_clean(src: &str, want: &str) {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-refmatch-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let result = (|| {
            if !available(&dir) {
                eprintln!("skipping: no ASan-capable cc");
                return None;
            }
            let file = dir.join("main.kara");
            std::fs::write(&file, src).expect("write source");
            Some(
                Command::new(env!("CARGO_BIN_EXE_karac"))
                    .arg("__mir-llvm-asan")
                    .arg(&file)
                    .output()
                    .expect("run karac"),
            )
        })();
        let _ = std::fs::remove_dir_all(&dir);
        let Some(out) = result else { return };
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.code() == Some(3) && err.contains("Linker failed") {
            let _ = super::common::link_or_skip(Err(err.into_owned()));
            return;
        }
        assert_eq!(out.status.code(), Some(0), "stderr:\n{err}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);
    }
}

/// The core pin runs, and runs clean under AddressSanitizer.
#[test]
fn mir_binds_a_match_of_ref_results_as_a_reference() {
    runs(
        include_str!("../corpus/core/ok_match_result_is_a_ref/source.kara"),
        include_str!("../corpus/core/ok_match_result_is_a_ref/expected.out"),
    );
}
