//! B-2026-10-05-85: named Option argument into a branch-reassigned rebind

use super::*;

/// B-2026-10-05-85: a NAMED `Option[R]` (or `Option[Array[R, 2]]`) argument passed
/// by value into a callee that rebinds it (`let mut h = x`) and then reassigns
/// the rebind on only SOME paths. The callee's local takes the value over, but the
/// named-argument box handover asked only whether the rebind was reassigned on a
/// straight line, so compiled the caller freed the box the callee had already
/// dropped: a double free. Call temporaries were covered by B-2026-10-04-50; this
/// is the named spelling, plus a match-arm reassign, an unconditional `None`, and
/// the plain-struct control.
#[test]
fn interp_named_option_arg_into_branch_reassigned_rebind_drops_once() {
    let out = run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn f1(x: Option[R], c: bool) { let mut h = x; if c { h = None; } println("f1"); }
fn f2(x: Option[Array[R, 2]], c: bool) { let mut h = x; if c { h = None; } println("f2"); }
fn f3(x: Option[R], n: i64) { let mut h = x; match n { 1 => { h = Some(mk(9)); } _ => {} } println("f3"); }
fn f4(x: Option[R]) { let mut h = x; h = None; println("f4"); }
fn f5(x: R, c: bool) { let mut h = x; if c { h = mk(8); } println("f5"); }
fn main() {
    let o = Some(mk(1)); f1(o, true); println("_a1");
    let o = Some(mk(2)); f1(o, false); println("_a2");
    f1(Some(mk(3)), true); println("_a3");
    let o: Option[Array[R, 2]] = Some([mk(4), mk(5)]); f2(o, true); println("_a4");
    let o: Option[Array[R, 2]] = Some([mk(6), mk(7)]); f2(o, false); println("_a5");
    let o = Some(mk(10)); f3(o, 1); println("_a6");
    let o = Some(mk(11)); f3(o, 2); println("_a7");
    let o = Some(mk(12)); f4(o); println("_a8");
    let a = mk(13); f5(a, true); println("_a9");
    println("end")
}
"#);
    assert_eq!(
        out,
        "dR1\nf1\n_a1\nf1\ndR2\n_a2\ndR3\nf1\n_a3\ndR4\ndR5\nf2\n_a4\nf2\ndR6\ndR7\n_a5\ndR10\nf3\ndR9\n_a6\nf3\ndR11\n_a7\ndR12\nf4\n_a8\ndR13\nf5\ndR8\n_a9\nend\n",
        "got:\n{out}"
    );
}
