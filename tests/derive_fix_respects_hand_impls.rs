//! B-2026-11-18-69: the implied-derive fix closed a derive list under §9's
//! chains without looking at the type's own impls, so `#[derive(PartialOrd)]`
//! on a type with a hand `impl PartialEq` was told `PartialOrd` needs
//! `PartialEq` and fixed to `#[derive(PartialOrd, PartialEq)]`, which is
//! E0240 (the type derives `PartialEq` and also implements it). A trait the
//! program implements by hand is no longer added.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// The sweep cell: an inherent `cmp` and a hand `PartialEq`, compared with `<`.
const CELL: &str = "struct P { x: i64 }\nimpl PartialEq for P { fn eq(ref self, other: ref P) -> bool { self.x == other.x } }\nimpl Eq for P {}\nimpl P { fn cmp(ref self, other: ref P) -> Ordering { self.x.cmp(other.x) } }\nfn main() {\n    let a = P { x: 1 };\n    let b = P { x: 5 };\n    println(f\"{a < b}\");\n}\n";

/// A derive whose chain the hand impl already covers: no warning, and the
/// hand `eq` is the one `==` calls.
const COVERED: &str = "#[derive(PartialOrd)]\nstruct P { x: i64 }\nimpl PartialEq for P { fn eq(ref self, other: ref P) -> bool { false } }\nfn main() {\n    let a = P { x: 1 };\n    let b = P { x: 2 };\n    println(a == a);\n    println(a < b);\n}\n";

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(src: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-derive-hand-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap().to_string();
    (dir, p)
}

/// `karac fix`, then the fixed program's check and its output on `lane`.
fn mend(src: &str, lane: &str) -> (String, String) {
    let (dir, p) = scratch(src);
    karac(&["fix", &p]);
    let fixed = std::fs::read_to_string(&p).unwrap();
    let check = karac(&["check", &p]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&[lane, &p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    (fixed, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn derive_fix_leaves_out_a_hand_implemented_trait() {
    let (fixed, out) = mend(CELL, "__mir-run");
    assert!(fixed.contains("#[derive(PartialOrd)]\n"), "{fixed}");
    assert!(!fixed.contains("PartialEq)]"), "{fixed}");
    assert_eq!(out, "true\n");
}

#[test]
fn a_hand_impl_covers_the_derive_chain() {
    let (dir, p) = scratch(COVERED);
    let out = karac(&["check", &p]);
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("implied derive"), "{text}");
    assert_eq!(mend(COVERED, "__mir-run").1, "false\ntrue\n");
}

#[cfg(feature = "llvm")]
#[test]
fn derive_fix_with_hand_impls_runs_compiled() {
    assert_eq!(mend(CELL, "__mir-llvm-run").1, "true\n");
    assert_eq!(mend(COVERED, "__mir-llvm-run").1, "false\ntrue\n");
}
