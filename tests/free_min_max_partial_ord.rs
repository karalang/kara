//! The free `min` and `max` take any `PartialOrd` pair of one type, floats
//! included, and two floats follow IEEE 754 `minNum` / `maxNum`, as the
//! methods `f64.min` / `f64.max` do: a NaN argument yields the other one
//! (review-thread ruling 2026-11-25).
//!
//! B-2026-11-24-48: `min(gap, 360.0 - gap)` on two `f64`s, kata 1344's
//! first draft, was refused with "`f64` does not implement `Ord`".
//!
//! B-2026-11-25-36: `min(1.5, 2.5)`, over float literals only, passed
//! `karac check` and stopped every lane with "call of unknown function
//! min". The bound was skipped while `T` was the literals' open variable,
//! and nothing checked it once the literals took `f64`. They now take it
//! and the bounds are checked then: an `Ord` one still refuses `f64`
//! (`clamp(2.5, 1.0, 2.0)`, a user's `fn g[T: Ord]`).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-free-min-max-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

/// `karac check` on `src`: whether it passed, and its output.
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

#[test]
fn kata_1344_free_min_max_on_two_f64s_runs() {
    runs_everywhere(
        "fn angle(h: f64, m: f64) -> f64 {\n\
         let gap = (30.0 * h - 5.5 * m).abs();\n\
         min(gap, 360.0 - gap)\n\
         }\n\
         fn main() {\n\
         let x: f32 = 2.5;\n\
         let top = max(x * 2.0, 1.5);\n\
         println(f\"{angle(3.0, 30.0)} {max(angle(12.0, 30.0), 1.5)} {top}\");\n\
         }\n",
        "75.0 165.0 5.0\n",
    );
}

#[test]
fn free_min_max_over_float_literals_only_runs() {
    runs_everywhere(
        "fn main() {\n\
         println(f\"{min(1.5, 2.5)} {max(1.5, 2.5)}\");\n\
         let y = 0.5;\n\
         let z = min(y, 2.5);\n\
         println(f\"{z} {min(2.5, y)} {max(y, 2.5)}\");\n\
         }\n",
        "1.5 2.5\n0.5 0.5 2.5\n",
    );
}

#[test]
fn a_nan_argument_yields_the_other_one() {
    runs_everywhere(
        "fn main() {\n\
         let nan = 0.0 / 0.0;\n\
         println(f\"{min(nan, 1.0)} {min(1.0, nan)} {max(nan, 1.0)} {max(1.0, nan)}\");\n\
         println(f\"{min(nan, nan)} {1.0.min(nan)} {nan.max(1.0)}\");\n\
         }\n",
        "1.0 1.0 1.0 1.0\nNaN 1.0 1.0\n",
    );
}

#[test]
fn a_derived_partial_ord_struct_takes_min_and_max() {
    runs_everywhere(
        "#[derive(PartialEq, PartialOrd)]\n\
         struct P { x: f64 }\n\
         fn main() {\n\
         let a = min(P { x: 3.0 }, P { x: 0.5 });\n\
         let b = max(P { x: 3.0 }, P { x: 0.5 });\n\
         println(f\"{a.x} {b.x} {min(3, 4)} {max(\"a\".to_string(), \"b\".to_string())}\");\n\
         }\n",
        "0.5 3.0 3 b\n",
    );
}

#[test]
fn an_ord_bound_still_refuses_float_literals_once_they_settle() {
    for src in [
        "fn main() { println(f\"{clamp(2.5, 1.0, 2.0)}\"); }\n",
        "fn g[T: Ord](a: own T, b: own T) -> T { a }\n\
         fn main() { let x = 0.5; println(f\"{g(x, 1.0)}\"); }\n",
    ] {
        let (ok, out) = check(src);
        assert!(!ok, "{src}\n{out}");
        assert!(
            out.contains("trait bound `T: Ord` is not satisfied; `f64` does not implement `Ord`"),
            "{src}\n{out}"
        );
    }
    let (ok, out) = check(
        "fn g[T: Ord](a: own T, b: own T) -> T { a }\n\
         fn main() { println(f\"{g(1, 2)} {clamp(5, 1, 3)}\"); }\n",
    );
    assert!(ok, "integer literals take `i64`, which is `Ord`:\n{out}");
}
