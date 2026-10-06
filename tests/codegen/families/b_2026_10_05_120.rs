//! B-2026-10-05-120: a fresh struct temp with an `Option[shared]` field handed back by its callee releases the handle once

use super::*;

/// B-2026-10-05-120 — a by-value param whose only droppable part is an
/// `Option[shared]` field (`struct W { o: Option[H], n: i64 }`) is ENTRY-COPIED:
/// the callee rc-increments the box, so its param holds a reference of its
/// own and a hand-back returns that one. The caller's entry-copy predicate
/// (`struct_type_is_entry_copied_heap`) did not count that field, read the
/// param as forwarded and stood a FRESH temp argument down, so the temp's own
/// reference was never released: every hand-back spelling below (a literal or
/// a call result, `return w`, a rebinding, a discarded call, a method, a
/// generic identity, a field store, a nested hand-back, an own-`Drop`
/// struct, an array of results, an `if` arm) leaked the 16 B box and never
/// ran `H`'s body on any compiled surface. `--interp` was right throughout;
/// the named argument (c15) was already right compiled.
#[test]
fn e2e_fresh_option_shared_struct_temp_handed_back_releases_once() {
    let Some(out) = run_program(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { o: Option[H], n: i64 }
struct D { o: Option[H], n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.n}") } }
struct G2 { w: W }
struct K { n: i64 }
impl K { fn pass(ref self, w: W) -> W { w } }
fn keepw(w: W) -> W { w }
fn retw(w: W) -> W { return w; }
fn rebw(w: W) -> W { let m = w; m }
fn id[T](x: T) -> T { x }
fn st(w: W) -> G2 { G2 { w: w } }
fn k2(w: W) -> W { keepw(w) }
fn keepd(d: D) -> D { d }
fn mkw(i: i64) -> W { W { o: Some(H { id: i }), n: i } }
fn c1() { let k = keepw(W { o: Some(H { id: 1 }), n: 1 }); println(f"_1 {k.n}"); }
fn c2() { let k = keepw(mkw(2)); println(f"_2 {k.n}"); }
fn c3() { let k = retw(W { o: Some(H { id: 3 }), n: 3 }); println(f"_3 {k.n}"); }
fn c4() { let k = rebw(W { o: Some(H { id: 4 }), n: 4 }); println(f"_4 {k.n}"); }
fn c5() { keepw(W { o: Some(H { id: 5 }), n: 5 }); println("_5"); }
fn c6() { let k = K { n: 0 }; let r = k.pass(W { o: Some(H { id: 6 }), n: 6 }); println(f"_6 {r.n}"); }
fn c7() { let mut v: Vec[W] = Vec.new(); v.push(keepw(W { o: Some(H { id: 7 }), n: 7 })); println(f"_7 {v.len()}"); }
fn c8() { let r = id(W { o: Some(H { id: 8 }), n: 8 }); println(f"_8 {r.n}"); }
fn c9() { let g = st(W { o: Some(H { id: 9 }), n: 9 }); println(f"_9 {g.w.n}"); }
fn c10() { let r = k2(W { o: Some(H { id: 10 }), n: 10 }); println(f"_10 {r.n}"); }
fn c11() { let r = keepd(D { o: Some(H { id: 11 }), n: 11 }); println(f"_11 {r.n}"); }
fn c12() { let r = keepw(keepw(W { o: Some(H { id: 12 }), n: 12 })); println(f"_12 {r.n}"); }
fn c13() { let ws = [keepw(mkw(13)), keepw(mkw(14))]; println(f"_13 {ws.len()}"); }
fn c14() { let c = true; let r = if c { keepw(mkw(15)) } else { mkw(16) }; println(f"_14 {r.n}"); }
fn c15() { let w = mkw(17); let k = keepw(w); println(f"_15 {k.n}"); }
fn main() {
    c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); c9(); c10(); c11(); c12(); c13(); c14(); c15();
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "_1 1\ndH1\n_2 2\ndH2\n_3 3\ndH3\n_4 4\ndH4\ndH5\n_5\n_6 6\ndH6\n_7 1\ndH7\n_8 8\ndH8\n_9 9\ndH9\n_10 10\ndH10\n_11 11\ndD11\ndH11\n_12 12\ndH12\n_13 2\ndH13\ndH14\n_14 15\ndH15\n_15 17\ndH17\nend\n", "got:\n{out}");
}
