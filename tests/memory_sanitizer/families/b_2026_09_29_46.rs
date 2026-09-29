//! B-2026-09-29-46 -- a NAMED `Option` argument to a conditional hand-back
//! callee: each path's box is freed exactly once.

use super::*;

/// B-2026-09-29-46 — the leak itself: the fresh box of the path that does not
/// hand the argument back was freed by nobody.
#[test]
fn asan_named_option_arg_to_conditional_handback_frees_each_box_once() {
    assert_clean_asan_run(
        r#"struct R { name: String, id: i64 }
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
"#,
        &[
            "dR2/b", "b0", "dR0/z", "ctrue", "dR3/c", "dR4/d", "e0", "dR0/z", "dR10/w", "v0",
            "dR0/z", "v11", "dR11/w", "mtrue", "r5", "dR5/q", "dR6/f", "f0", "dR0/z", "end",
        ],
        "B-2026-09-29-46 named Option arg to a conditional hand-back",
    );
}
