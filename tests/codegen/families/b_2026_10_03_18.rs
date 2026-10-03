//! B-2026-10-03-18 -- `replace(d, v)` through a wrapper ran the moved
//! param's `Drop` body twice on every backend.

use super::*;

/// B-2026-10-03-18 — neither backend read `replace(d, v)` as a store of `v`
/// into the outliving place `d` names, so the caller ran `v`'s body at the
/// end of the call while `*d` went on to run it again (`dR2 1 dR1 2 dR2`
/// everywhere). Both outliving-store walkers now recognise it, bare, under
/// `return` and as a `let` value. Covers the wrapper, a conditional replace
/// whose other path leaves the param to die, and a statement-level one.
#[test]
fn e2e_replace_through_wrapper_runs_moved_param_body_once() {
    let src = r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn rep(d: mut ref R, v: R) -> R { return replace(d, v) }
fn repc(d: mut ref R, v: R, c: bool) -> i64 {
    if c { let o = replace(d, v); return o.id }
    return 0
}
fn put(d: mut ref R, v: R) { let o = replace(d, v); println(o.id); }
fn main() {
    let mut r = mk(1);
    let old = rep(mut r, mk(2));
    println(old.id);
    println(r.id);
    let k1 = repc(mut r, mk(3), true);
    println(k1);
    let k2 = repc(mut r, mk(4), false);
    println(k2);
    put(mut r, mk(5));
    println(r.id);
    println("end");
}
"#;
    let want = "1\ndR1\n2\ndR2\n2\ndR4\n0\n3\ndR3\n5\ndR5\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
