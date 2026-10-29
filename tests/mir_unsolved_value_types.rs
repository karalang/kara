//! A value whose type the checker left with an unsolved part, lowered as
//! `()`, in a slot whose type names it (B-2026-10-29-25): the `None` of
//! `Vec.filled(3, None)` pushed into a `Vec[Vec[Option[i64]]]`, or of
//! `vec![None; 4]`. The value is built at the slot's type, so the element
//! the library copies has the slot's size; compiled, it was an
//! `Option[()]` read as an `Option[i64]`, past the end of its stack slot.
//! Outputs are worked out by hand.
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
        let dir = std::env::temp_dir().join(format!("karac-holes-{}-{n}", std::process::id()));
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

/// The row's program: `Vec.filled(n, None)` pushed into a vector of
/// vectors of options.
#[test]
fn mir_builds_a_filled_none_at_its_element_type() {
    runs(
        r#"fn main() {
    let mut d: Vec[Vec[Option[i64]]] = Vec.new();
    d.push(Vec.filled(3, None));
    println(f"{d[0].len()} {d[0][2].is_none()}");
}
"#,
        "3 true\n",
    );
}

/// The same with an owning payload, a repeat literal, and a tuple
/// payload: each element is written, read and dropped at its real size.
#[test]
fn mir_builds_other_unsolved_nones_at_their_slot_types() {
    runs(
        r#"fn main() {
    let mut d: Vec[Vec[Option[i64]]] = Vec.new();
    d.push(Vec.filled(3, None));
    d[0][1] = Some(7);
    let mut e: Vec[Vec[Option[Vec[u8]]]] = Vec.new();
    e.push(Vec.filled(2, None));
    e[0][0] = Some(vec![1, 2]);
    let f: Vec[Option[(i64, String)]] = vec![None; 3];
    let mut g: Vec[Vec[Option[i64]]] = Vec.new();
    g.push(vec![None; 4]);
    println(f"{d[0].len()} {d[0][2].is_none()} {d[0][1]} {e[0].len()} {e[0][0]} {f.len()} {g[0].len()} {g[0][3].is_none()}");
}
"#,
        "3 true Some(7) 2 Some([1, 2]) 3 4 true\n",
    );
}
