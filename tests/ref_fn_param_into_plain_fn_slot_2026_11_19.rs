//! B-2026-11-19-141: a `f: mut ref MutFn(..)` parameter handed to `any`
//! was refused "expected 'Fn(i64) -> bool', found 'mut ref Fn(i64) ->
//! bool'": the slot is a plain function type, which -106's fn-bound solving
//! does not reach. A `ref`/`mut ref` to a function value now checks as the
//! function it reaches for such a slot, and MIR forwards over the reference.
//! The checker's function type carries no kind, so the kind is the
//! ownership pass's: a `MutFn` into an `Fn` slot, and a `MutFn` behind a
//! shared borrow into any function slot, are refused (E0518), for a plain
//! slot and a fn-bound generic one alike.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <cmd> main.kara` on `src`; returns (exit code, stdout, stderr).
fn karac(src: &str, cmd: &str) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-reffnslot-{}-{n}", std::process::id()));
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
fn mut_ref_mutfn_param_into_any_keeps_the_closures_writes() {
    let src = "fn has(v: ref Vec[i64], f: mut ref MutFn(i64) -> bool) -> bool { v.iter().cloned().any(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + 1; x == 2 }; println(has([1, 2, 3], mut g)); println(n); }
";
    let (code, out, err) = karac(src, "__mir-run");
    assert_eq!((code, out.as_str()), (Some(0), "true\n2\n"), "{err}");
}

#[test]
fn ref_fn_param_into_a_user_fn_slot_is_called_through_the_reference() {
    let src = "fn apply(f: Fn(i64) -> i64) -> i64 { f(3) }
fn via(f: ref Fn(i64) -> i64) -> i64 { apply(f) }
fn main() { let h = |x: i64| x * 2; println(via(h)); }
";
    let (code, out, err) = karac(src, "__mir-run");
    assert_eq!((code, out.as_str()), (Some(0), "6\n"), "{err}");
}

#[test]
fn mut_ref_mutfn_param_into_an_fn_slot_is_a_kind_mismatch() {
    let src = "fn apply(f: Fn(i64) -> i64) -> i64 { f(3) }
fn via(f: mut ref MutFn(i64) -> i64) -> i64 { apply(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + x; n }; println(via(mut g)); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]") && err.contains("`f` is a MutFn, but apply expects Fn"),
        "{err}"
    );
}

#[test]
fn mut_ref_mutfn_param_into_an_fn_bound_is_a_kind_mismatch() {
    let src = "fn ap[F: Fn(i64) -> i64](f: F) -> i64 { f(1) }
fn via(f: mut ref MutFn(i64) -> i64) -> i64 { ap(f) }
fn main() { let mut n = 0; let mut g = |x: i64| { n = n + x; n }; println(via(mut g)); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]") && err.contains("`f` is a MutFn, but ap expects Fn"),
        "{err}"
    );
}

#[test]
fn shared_ref_mutfn_param_into_a_mutfn_bound_is_refused() {
    let src = "fn cnt(v: ref Vec[i64], f: ref MutFn(i64) -> bool) -> i64 { v.iter().cloned().filter(f).count() }
fn main() { let mut n = 0; let g = |x: i64| { n = n + 1; x == 2 }; println(cnt([1, 2], g)); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]") && err.contains("`f` is a `MutFn` held by a shared borrow"),
        "{err}"
    );
}

#[test]
fn shared_ref_mutfn_param_into_an_own_mutfn_slot_is_refused_with_a_mut_ref_fix() {
    let src = "fn twice(f: own MutFn(i64) -> i64) -> i64 { f(3) + f(4) }
fn via(f: ref MutFn(i64) -> i64) -> i64 { twice(f) }
fn main() { let mut n = 0; let g = |x: i64| { n = n + x; n }; println(via(g)); }
";
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0518]") && err.contains("Declare the parameter `f: mut ref ...`"),
        "{err}"
    );
}
