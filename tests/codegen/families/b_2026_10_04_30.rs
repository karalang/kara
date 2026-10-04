//! B-2026-10-04-30 — `t.unwrap().id` over a named `Option[shared]` place (a
//! by-value param, a local, a field of one, `self`) released the payload
//! after loading the field as if `unwrap` had returned a fresh `+1`
//! temporary. It returns the place's own pointer, and the place keeps its own
//! release, so that was one ref released twice: the `Drop` body ran before the
//! read was printed and the field was read out of a freed object.

use super::*;

/// B-2026-10-04-30 — temporary and named arguments, `expect`, a field of a
/// by-value struct param, a local read twice, a local struct's field read
/// twice, a `ref self` method called twice, and an aliased handle. The
/// `let u = t.unwrap()` and `match` cells are controls that were already
/// clean.
///
/// The interpreter loses the `Drop` body of every handle inside a fresh
/// `Option` temporary passed by value (`dH1`, `dH4`, `dH8`, `dH9`):
/// PREDICTS B-2026-09-27-92, whose fix makes its line equal the compiled one.
#[test]
fn e2e_unwrap_field_read_on_named_option_shared_place_keeps_the_ref() {
    let src = r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { o: Option[H], n: i64 }
impl W { fn r(ref self) { println(f"r{self.o.unwrap().id}") } }
fn ch(t: Option[H]) { println(f"h{t.unwrap().id}") }
fn chx(t: Option[H]) { println(f"x{t.expect("none").id}") }
fn cw(w: W) { println(f"w{w.o.unwrap().id} {w.n}") }
fn cu(t: Option[H]) { let u = t.unwrap(); println(f"u{u.id}") }
fn cm(t: Option[H]) { match t { Some(h) => println(f"m{h.id}"), None => {} } }
fn main() {
    { ch(Some(H { id: 1 })); println("e1"); }
    { let p = Some(H { id: 2 }); ch(p); println("e2"); }
    { let o = Some(H { id: 3 }); println(f"l{o.unwrap().id}"); println(f"l{o.unwrap().id}"); println("e3"); }
    { chx(Some(H { id: 4 })); println("e4"); }
    { let p = Some(H { id: 5 }); chx(p); println("e5"); }
    { cw(W { o: Some(H { id: 6 }), n: 7 }); println("e6"); }
    { cu(Some(H { id: 8 })); println("e7"); }
    { cm(Some(H { id: 9 })); println("e8"); }
    { let h = H { id: 10 }; let k = h; ch(Some(h)); println(f"k{k.id}"); println("e9"); }
    { let w = W { o: Some(H { id: 11 }), n: 1 }; println(f"w{w.o.unwrap().id}"); println(f"w{w.o.unwrap().id}"); }
    { let w = W { o: Some(H { id: 12 }), n: 1 }; w.r(); w.r(); }
    println("end")
}"#;
    let want = "h1\ndH1\ne1\nh2\ne2\ndH2\nl3\nl3\ne3\ndH3\nx4\ndH4\ne4\nx5\ne5\ndH5\nw6 7\ndH6\ne6\nu8\ndH8\ne7\nm9\ndH9\ne8\nh10\nk10\ndH10\ne9\nw11\nw11\ndH11\nr12\nr12\ndH12\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(
        interp_out.join(""),
        "h1\ne1\nh2\ne2\ndH2\nl3\nl3\ne3\ndH3\nx4\ne4\nx5\ne5\ndH5\nw6 7\ndH6\ne6\nu8\ne7\nm9\ne8\nh10\nk10\ndH10\ne9\nw11\nw11\ndH11\nr12\nr12\ndH12\nend\n",
        "interpreter"
    );
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
