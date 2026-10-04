//! B-2026-10-02-93 — `let g = h;` over a non-shared enum local that is read
//! again afterwards emptied `h` on the compiled backends. The ASAN twin of
//! the codegen fixture: the copy `g` now gets and the original `h` keeps
//! must each be freed exactly once.

use super::*;

/// B-2026-10-02-93 — the codegen fixture's program under ASAN.
#[test]
fn asan_enum_let_move_then_reuse_keeps_the_source_payload() {
    assert_clean_asan_run_min_allocs(
        r#"enum Hc { Full(String), Empty }
enum Hv { Vs(Vec[String]), Vi(Vec[i64]), E }
struct W { s: String, n: i64 }
enum Hw { Wr(W), E }
shared struct Node { v: i64 }
struct Wn { n: Node, s: String }
enum Hn { Wn(Wn), E }
enum Hm { M(Map[i64, String]), E }
enum Ht { T((String, i64)), E }
enum Hk { K { a: String, b: Vec[String] }, E }
fn hs(s: String) -> String { s + "-heap-string-long-enough" }
fn gr(h: ref Hc) -> i64 { match h { Hc.Full(s) => s.len(), Hc.Empty => 0 } }
fn gv(h: ref Hv) -> i64 { match h { Hv.Vs(v) => v.len() * 100 + v[0].len(), Hv.Vi(v) => v.len() * 100 + v[0], Hv.E => 0 } }
fn gw(h: ref Hw) -> i64 { match h { Hw.Wr(w) => w.s.len() + w.n, Hw.E => 0 } }
fn gn(h: ref Hn) -> i64 { match h { Hn.Wn(w) => w.s.len() + w.n.v, Hn.E => 0 } }
fn gm(h: ref Hm) -> i64 { match h { Hm.M(m) => m.len(), Hm.E => 0 } }
fn gt(h: ref Ht) -> i64 { match h { Ht.T(t) => t.0.len() + t.1, Ht.E => 0 } }
fn gk(h: ref Hk) -> i64 { match h { Hk.K { a, b } => a.len() + b.len(), Hk.E => 0 } }
fn eat(h: Hc) -> i64 { gr(h) }
struct Box1 { h: Hc }
fn main() {
    { let h = Hc.Full(hs("ab")); let g = h; println(f"c1 {gr(g)} {gr(h)}"); }
    { let h = Hv.Vs([hs("a"), hs("b")]); let g = h; println(f"c2 {gv(g)} {gv(h)}"); }
    { let h = Hv.Vi([7, 8]); let g = h; println(f"c3 {gv(g)} {gv(h)}"); }
    { let h = Hw.Wr(W { s: hs("w"), n: 3 }); let g = h; println(f"c4 {gw(g)} {gw(h)}"); }
    { let h = Hn.Wn(Wn { n: Node { v: 5 }, s: hs("n") }); let g = h; println(f"c5 {gn(g)} {gn(h)}"); }
    { let mut m: Map[i64, String] = Map.new(); m.insert(1, hs("m")); let h = Hm.M(m); let g = h; println(f"c6 {gm(g)} {gm(h)}"); }
    { let h = Ht.T((hs("t"), 2)); let g = h; println(f"c8 {gt(g)} {gt(h)}"); }
    { let h = Hk.K { a: hs("k"), b: [hs("x")] }; let g = h; println(f"c10 {gk(g)} {gk(h)}"); }
    { let h = Hc.Full(hs("ab")); let n = eat(h); println(f"c11 {n} {gr(h)}"); }
    { let h = Hc.Full(hs("ab")); let b = Box1 { h: h }; println(f"c12 {gr(b.h)} {gr(h)}"); }
    { let h = Hc.Empty; let g = h; println(f"c13 {gr(g)} {gr(h)}"); }
    { let h = Hc.Full(hs("ab")); let g = h; let k = h; println(f"c15 {gr(g)} {gr(k)} {gr(h)}"); }
    { let mut v: Vec[Hc] = Vec.new(); let h = Hc.Full(hs("ab")); v.push(h); println(f"c16 {gr(v[0])} {gr(h)}"); }
    println("end")
}"#,
        &[
            "c1 26 26",
            "c2 225 225",
            "c3 207 207",
            "c4 28 28",
            "c5 30 30",
            "c6 1 1",
            "c8 27 27",
            "c10 26 26",
            "c11 26 26",
            "c12 26 26",
            "c13 0 0",
            "c15 26 26 26",
            "c16 26 26",
            "end",
        ],
        "asan_enum_let_move_then_reuse_keeps_the_source_payload",
        20,
    );
}
