//! B-2026-11-19-103: a parameter borrows, so `v.iter().filter(f)` with a
//! function-typed parameter `f` hands the adaptor a borrowed function
//! value. It goes in as `|x| f(x)` would (MIR lowering's `erase_operand`),
//! and was E0516 'cannot be stored as a function value'. What keeps `f` past
//! the function stays refused (core semantics §9.3), now also through a
//! `let`: an adaptor or closure bound to a local holds `f`, and returning
//! or storing that local is E0200, as returning the adaptor directly is.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <cmd> main.kara` on `src`; returns (exit code, stdout, stderr).
fn karac(src: &str, cmd: &str) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-fnfwd-{}-{n}", std::process::id()));
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

fn refused(src: &str, holder: &str) {
    let (code, _, err) = karac(src, "check");
    assert_ne!(code, Some(0), "{err}");
    assert!(err.contains("error[E0200]"), "{err}");
    assert!(err.contains(holder), "{err}");
    assert!(err.contains("`f: escaping Fn(...)`"), "{err}");
}

#[test]
fn fn_param_into_filter_runs_as_a_forwarding_closure() {
    let src =
        "fn cnt(v: ref Vec[i64], f: Fn(ref i64) -> bool) -> i64 { v.iter().filter(f).count() }
fn main() { println(cnt([1, 2, 3], |x| *x != 2)); }
";
    let (code, out, err) = karac(src, "run");
    assert_eq!((code, out.as_str()), (Some(0), "2\n"), "{err}");
}

#[test]
fn fn_param_through_option_map_bound_to_a_local_is_not_held() {
    // `Option.map` runs its function at once and keeps nothing.
    let src = "fn ap(o: Option[i64], f: Fn(i64) -> i64) -> Option[i64] { let r = o.map(f); r }
fn main() { println(ap(Some(2), |x| x + 1)); }
";
    let (code, out, err) = karac(src, "run");
    assert_eq!((code, out.as_str()), (Some(0), "Some(3)\n"), "{err}");
}

#[test]
fn fn_param_in_a_let_bound_adaptor_stored_in_a_field_is_refused() {
    refused(
        "struct Hold { it: Option[IterFilter[VecIntoIter[i64], Fn(ref i64) -> bool]] }
fn put(h: mut ref Hold, f: Fn(ref i64) -> bool) { let it = [1, 2, 3].into_iter().filter(f); h.it = Some(it); }
fn main() { let mut h = Hold { it: None }; put(mut h, |x| *x != 2); println(h.it.unwrap().count()); }
",
        "`it` holds parameter `f`",
    );
}

#[test]
fn fn_param_in_a_let_bound_written_forwarder_stored_in_a_field_is_refused() {
    refused(
        "struct Hold { it: Option[IterFilter[VecIntoIter[i64], Fn(ref i64) -> bool]] }
fn put(h: mut ref Hold, f: Fn(ref i64) -> bool) { let it = [1, 2, 3].into_iter().filter(|x| f(x)); h.it = Some(it); }
fn main() { let mut h = Hold { it: None }; put(mut h, |x| *x != 2); println(h.it.unwrap().count()); }
",
        "`it` holds parameter `f`",
    );
}

#[test]
fn fn_param_through_a_chain_of_let_bound_adaptors_returned_is_refused() {
    refused(
        "fn mk(v: own Vec[i64], f: Fn(ref i64) -> bool) -> IterMap[IterFilter[VecIntoIter[i64], Fn(ref i64) -> bool], Fn(i64) -> i64] {
    let it = v.into_iter().filter(f);
    let m = it.map(|x| x + 1);
    m
}
fn main() { println(mk([1, 2, 3], |x| *x != 2).count()); }
",
        "`m` holds parameter `f`",
    );
}

#[test]
fn fn_param_captured_by_a_let_bound_closure_pushed_is_refused() {
    refused(
        "fn put(out: mut ref Vec[Fn(i64) -> i64], f: Fn(i64) -> i64) { let g = |x| f(x); out.push(g); }
fn main() { let mut v: Vec[Fn(i64) -> i64] = []; put(mut v, |x| x + 1); println(v[0](2)); }
",
        "`g` captures parameter `f`",
    );
}
