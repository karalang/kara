//! B-2026-11-20-9: a bare `f: MutFn(..)` parameter is borrowed (D5), and
//! calling it is refused as "a borrowed MutFn", but handing it to an `own
//! MutFn` slot (`it.any(f)`, a user `own MutFn` parameter) was accepted, so
//! the callee called a `MutFn` through a shared borrow. It is now refused
//! with the same rule (E0518) and a fix that declares the parameter
//! `mut ref`; a bare, borrowed slot still takes it, since it cannot call it.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <args> main.kara` on `src`; returns (exit code, stdout,
/// stderr, the file afterwards).
fn karac(src: &str, args: &[&str]) -> (Option<i32>, String, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-bmutfn-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let after = std::fs::read_to_string(dir.join("main.kara")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        after,
    )
}

const ANY: &str = "fn has(v: ref Vec[i64], f: MutFn(i64) -> bool) -> bool { v.iter().cloned().any(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + 1; x == 2 }; println(has([1, 2, 3], g)); println(n); }
";

#[test]
fn bare_mutfn_param_into_any_is_refused() {
    let (code, _, err, _) = karac(ANY, &["check"]);
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]")
            && err.contains("`f` is a borrowed `MutFn`, so it cannot be passed"),
        "{err}"
    );
}

#[test]
fn bare_mutfn_param_into_any_is_fixed_to_a_mut_ref_that_runs() {
    let (_, _, err, after) = karac(ANY, &["fix"]);
    assert!(
        after.contains("f: mut ref MutFn(i64) -> bool") && after.contains("has([1, 2, 3], mut g)"),
        "{err}\n{after}"
    );
    let (code, out, err, _) = karac(&after, &["__mir-run"]);
    assert_eq!((code, out.as_str()), (Some(0), "true\n2\n"), "{err}");
}

#[test]
fn bare_mutfn_param_into_a_user_own_mutfn_param_is_refused() {
    let src = "fn twice(f: own MutFn(i64) -> i64) -> i64 { f(1) + f(2) }
fn via(f: MutFn(i64) -> i64) -> i64 { twice(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + x; n }; println(via(g)); }
";
    let (code, _, err, _) = karac(src, &["check"]);
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]")
            && err.contains("`f` is a borrowed `MutFn`, so it cannot be passed"),
        "{err}"
    );
}

#[test]
fn bare_mutfn_param_into_a_bare_mutfn_param_is_accepted() {
    let src = "fn keep(f: MutFn(i64) -> i64) -> i64 { 7 }
fn via(f: MutFn(i64) -> i64) -> i64 { keep(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + x; n }; println(via(g)); }
";
    let (code, _, err, _) = karac(src, &["check"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(!err.contains("cannot be passed"), "{err}");
}
