//! B-2026-10-04-30 — the ASAN twin of the codegen fixture: `unwrap()` on a
//! named `Option[shared]` place no longer releases the place's own ref after
//! the field read, so no handle is read after its free or released twice.

use super::*;

/// B-2026-10-04-30 — the codegen fixture's program under ASAN.
#[test]
fn asan_unwrap_field_read_on_named_option_shared_place_keeps_the_ref() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
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
}"#,
        &[
            "h1", "dH1", "e1", "h2", "e2", "dH2", "l3", "l3", "e3", "dH3", "x4", "dH4", "e4", "x5",
            "e5", "dH5", "w6 7", "dH6", "e6", "u8", "dH8", "e7", "m9", "dH9", "e8", "h10", "k10",
            "dH10", "e9", "w11", "w11", "dH11", "r12", "r12", "dH12", "end",
        ],
        "asan_unwrap_field_read_on_named_option_shared_place_keeps_the_ref",
        12,
    );
}
