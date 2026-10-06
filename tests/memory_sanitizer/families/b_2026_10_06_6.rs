//! B-2026-10-06-6: an owned-`self` method handing back its struct receiver whole runs each body once

use super::*;

/// B-2026-10-06-6 / B-2026-09-27-11 — an owned-`self` method that hands its
/// struct receiver back whole (`fn me(self) -> X { self }`, the `let m = self;
/// m` and `return self` spellings). A struct with a direct `shared` field is
/// FORWARDED, so the result and the receiver's slot held one handle and both
/// released it: an invalid read and write of the freed RC block on every
/// compiled spelling (c1..c13), `malloc(): unaligned tcache chunk detected`
/// for the whole program. The interpreter lost `dH2` and `dH6` (c2, c6) and
/// ran a `Drop` body twice wherever the struct has one (c14..c17). A
/// copy-supported struct (`S`, `P` over `R`) ran every body twice on all four
/// surfaces (c18..c21, c22), B-2026-09-27-11's cells. Each value now runs its
/// bodies once, in a branch either way (c9, c15, c22, c23) and through a
/// reassignment (c10, c11, c16, c24, c25).
#[test]
fn asan_owned_self_method_handing_back_its_struct_receiver_runs_each_body_once() {
    assert_clean_asan_run(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct X { h: H, n: i64 }
impl X {
    fn me(self) -> X { self }
    fn reb(self) -> X { let m = self; m }
    fn ret(self) -> X { return self; }
}
fn mkx(i: i64) -> X { X { h: H { id: i }, n: i } }
struct D { h: H, n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.n}") } }
impl D { fn me(self) -> D { self } }
struct N { kids: Vec[N], n: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.n}") } }
impl N { fn me(self) -> N { self } }
struct W { x: X }
impl X { fn wrap(self) -> W { W { x: self } } }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { R { id: i } }
struct S { r: R }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.r.id}") } }
impl S { fn ret_self(self) -> S { return self } }
struct P { r: R }
impl P { fn ret_self(self) -> P { return self } }
fn c1() { let x = mkx(1); let b = x.me(); println(f"_{b.n}"); }
fn c2() { let x = mkx(2); let b = x.reb(); println(f"_{b.n}"); }
fn c3() { let x = mkx(3); let b = x.ret(); println(f"_{b.n}"); }
fn c4() { let b = X { h: H { id: 4 }, n: 4 }.me(); println(f"_{b.n}"); }
fn c5() { let b = mkx(5).me(); println(f"_{b.n}"); }
fn c6() { X { h: H { id: 6 }, n: 6 }.me(); println("_6"); }
fn c7() { let x = mkx(7); x.me(); println("_7"); }
fn c8() { let x = mkx(8); let b = x.me().me(); println(f"_{b.n}"); }
fn c9() { let x = mkx(9); let c = x.n < 100; if c { let b = x.me(); println(f"_{b.n}"); } println("_9"); }
fn c10() { let mut x = mkx(10); x = x.me(); println(f"_{x.n}"); }
fn c11() { let mut x = mkx(11); for i in 0..3 { x = x.me(); } println(f"_{x.n}"); }
fn c12() { let x = mkx(12); let v = vec![x.me()]; println(f"_{v[0].n}"); }
fn c13() { let x = mkx(13); let w = x.wrap(); println(f"_{w.x.n}"); }
fn c14() { let x = D { h: H { id: 14 }, n: 14 }; let b = x.me(); println(f"_{b.n}"); }
fn c15() { let d = D { h: H { id: 15 }, n: 15 }; let c = d.n < 100; if c { let b = d.me(); println(f"_{b.n}"); } println("_15"); }
fn c16() { let mut d = D { h: H { id: 16 }, n: 16 }; d = d.me(); println(f"_{d.n}"); }
fn c17() { let x = N { kids: Vec.new(), n: 17 }; let b = x.me(); println(f"_{b.n}"); }
fn c18() { let a = S { r: mk(18) }; let b = a.ret_self(); println("_18"); }
fn c19() { let a = P { r: mk(19) }; let b = a.ret_self(); println("_19"); }
fn c20() { let a = S { r: mk(20) }; a.ret_self(); println("_20"); }
fn c21() { let a = S { r: mk(21) }; let b = a.ret_self().ret_self(); println("_21"); }
fn c22() { let a = S { r: mk(22) }; let c = a.r.id < 100; if c { let b = a.ret_self(); println("in"); } println("_22"); }
fn c23() { let a = S { r: mk(23) }; let c = a.r.id > 100; if c { let b = a.ret_self(); println("in"); } println("_23"); }
fn c24() { let mut a = P { r: mk(24) }; a = a.ret_self(); println("_24"); }
fn c25() { let mut a = S { r: mk(25) }; for i in 0..2 { a = a.ret_self(); } println("_25"); }
fn main() {
    c1(); c2(); c3(); c4(); c5(); c6(); c7(); c8(); c9(); c10(); c11(); c12(); c13();
    c14(); c15(); c16(); c17(); c18(); c19(); c20(); c21(); c22(); c23(); c24(); c25();
    println("end")
}
"#,
        &[
            "_1", "dH1", "_2", "dH2", "_3", "dH3", "_4", "dH4", "_5", "dH5", "dH6", "_6", "dH7",
            "_7", "_8", "dH8", "_9", "dH9", "_9", "_10", "dH10", "_11", "dH11", "_12", "dH12",
            "_13", "dH13", "_14", "dD14", "dH14", "_15", "dD15", "dH15", "_15", "_16", "dD16",
            "dH16", "_17", "dN17", "dS18", "dR18", "_18", "dR19", "_19", "dS20", "dR20", "_20",
            "dS21", "dR21", "_21", "dS22", "dR22", "in", "_22", "dS23", "dR23", "_23", "dR24",
            "_24", "dS25", "dR25", "_25", "end",
        ],
        "asan_owned_self_method_handing_back_its_struct_receiver_runs_each_body_once",
    );
}
