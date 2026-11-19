//! B-2026-11-19-106: a `f: mut ref MutFn(..)` parameter handed to `filter`
//! was refused 'trait bound `FnV: MutFn` is not satisfied; `mut ref Fn(..)`
//! does not implement `MutFn`'. A reference to a function value for a type
//! parameter with a function-trait bound now solves it as the function it
//! reaches (a `ref` for any kind, a `mut ref` for `MutFn` and `OnceFn`), and
//! MIR hands the slot `|x| f(x)` over the reference. The adaptor holds the
//! reborrow, so one that outlives the closure is refused at the caller.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <cmd> main.kara` on `src`; returns (exit code, stdout, stderr).
fn karac(src: &str, cmd: &str) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-mutfnfwd-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn mut_ref_mutfn_param_into_filter_keeps_the_closures_writes() {
    let src = "fn cnt(v: ref Vec[i64], f: mut ref MutFn(ref i64) -> bool) -> i64 { v.iter().filter(f).count() }
fn main() { let mut n = 0; let mut g = |x: ref i64| { n += 1; *x > 1 }; println(cnt([1, 2, 3], mut g)); println(n); }
";
    let (code, out, err) = karac(src, "run");
    assert_eq!((code, out.as_str()), (Some(0), "2\n3\n"), "{err}");
}

#[test]
fn mut_ref_mutfn_adaptor_outliving_its_closure_is_refused_at_the_caller() {
    let src = "fn mk(v: own Vec[i64], f: mut ref MutFn(ref i64) -> bool) -> IterFilter[VecIntoIter[i64], MutFn(ref i64) -> bool] { v.into_iter().filter(f) }
fn main() { let it = { let mut g = |x: ref i64| *x != 2; mk([1, 2, 3], mut g) }; println(it.count()); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(err.contains("error[E0516]"), "{err}");
    assert!(
        err.contains("cannot end `g` while `it` still holds a borrow of it"),
        "{err}"
    );
}

#[test]
fn mut_ref_mutfn_param_for_an_fn_bound_is_still_refused() {
    let src = "fn ap[F: Fn(i64) -> i64](f: own F) -> i64 { f(1) }
fn go(f: mut ref MutFn(i64) -> i64) -> i64 { ap(mut f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n += x; n }; println(go(mut g)); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(err.contains("does not implement `Fn`"), "{err}");
}
