//! B-2026-09-29-46 -- a NAMED `Option` argument to a callee that hands it back
//! on SOME exits: the result owns whichever box it holds, once.

use super::*;

/// B-2026-09-29-46 — `let b = h0(x, false)` over `fn h0(o: Option[R], f: bool)
/// -> Option[R] { if f { return o } return Some(mk(0)) }` skipped `b`'s box
/// drop (the result might be `x`'s box), so the fresh box of the other path
/// leaked 32 B on every compiled surface. Covered: both paths, a second hop,
/// a loop over both, a `None` argument, an unconditional identity, and a
/// fresh-temp argument.
#[test]
fn e2e_named_option_arg_to_conditional_handback_frees_each_box_once() {
    let src = r#"struct R { name: String, id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.name}") } }
fn mk(n: i64, s: String) -> R { R { name: s, id: n } }
fn h0(o: Option[R], f: bool) -> Option[R] { if f { return o } return Some(mk(0, f"z")) }
fn idn(o: Option[R]) -> Option[R] { o }
fn eat(o: Option[R]) -> i64 { match o { Some(r) => r.id, None => -1 } }
fn main() {
    let x = Some(mk(2, f"b"));
    let b = h0(x, false);
    println(f"b{eat(b)}");
    let y = Some(mk(3, f"c"));
    let c = h0(y, true);
    println(f"c{c.is_some()}");
    let z = Some(mk(4, f"d"));
    let d = h0(z, false);
    let e = h0(d, true);
    println(f"e{eat(e)}");
    for i in 0..2 {
        let w = Some(mk(10 + i, f"w"));
        let v = h0(w, i == 1);
        println(f"v{eat(v)}");
    }
    let n: Option[R] = None;
    let m = h0(n, true);
    println(f"m{m.is_none()}");
    let q = Some(mk(5, f"q"));
    let r = idn(q);
    println(f"r{eat(r)}");
    let f = h0(Some(mk(6, f"f")), false);
    println(f"f{eat(f)}");
    println("end")
}
"#;
    let want = "dR2/b\nb0\ndR0/z\nctrue\ndR3/c\ndR4/d\ne0\ndR0/z\ndR10/w\nv0\ndR0/z\nv11\ndR11/w\nmtrue\nr5\ndR5/q\ndR6/f\nf0\ndR0/z\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
