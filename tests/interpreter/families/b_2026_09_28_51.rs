//! B-2026-09-28-51 — a fresh temporary struct handed to a by-value callee, or
//! discarded, runs its `shared` field's `Drop` body under `--interp`.

use super::*;

/// B-2026-09-28-51 — `zn(mkz(1))` over `struct Z { d: D, h: N }` (`N` a
/// `shared struct` with a `Drop` body) ran `D`'s body and never `N`'s under
/// `--interp`, where every compiled surface ran both: a temporary has no
/// binding whose death releases a shared field. The same loss covered a
/// discarded `mkz(14);` / `let _ = mkz(17);`, a struct with its own `Drop`
/// (`wn(mkw(12))`) and a shared-only struct LITERAL (`yn(Y { .. })`). Each
/// fires at compiled's point: with the own body, or at the statement's end.
#[test]
fn test_fresh_temp_struct_arg_runs_its_shared_field_body() {
    let out = run(r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
shared struct N { v: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.v}") } }
struct Z { d: D, h: N }
struct Y { h: N, k: i64 }
struct W { h: N, k: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.k}") } }
struct Q { z: Z, t: i64 }
struct P { a: N, b: N }
struct H { k: i64 }
impl H { fn m(ref self, z: Z) -> i64 { z.h.v } }
fn mkz(n: i64) -> Z { Z { d: mkd(n), h: N { v: n } } }
fn mkw(n: i64) -> W { W { h: N { v: n }, k: n } }
fn mky(n: i64) -> Y { Y { h: N { v: n }, k: n } }
fn zn(z: Z) -> i64 { z.h.v }
fn yn(y: Y) -> i64 { y.k }
fn wn(w: W) -> i64 { w.k }
fn back(z: Z) -> Z { z }
fn keep(zs: mut ref Vec[Z], z: Z) { zs.push(z); }
fn qn(q: Q) -> i64 { q.t }
fn pn(p: P) -> i64 { p.a.v + p.b.v }
fn main() {
    println(f"a{zn(mkz(1))}");
    println(f"b{zn(Z { d: mkd(2), h: N { v: 2 } })}");
    let z3 = mkz(3); println(f"c{zn(z3)}");
    println(f"d{yn(Y { h: N { v: 4 }, k: 4 })}");
    let h = H { k: 0 };
    println(f"e{h.m(mkz(5))}");
    let b = back(mkz(6)); println(f"f{b.h.v}");
    let mut zs: Vec[Z] = Vec.new(); keep(mut zs, mkz(7)); println(f"g{zs.len()}");
    println(f"h{qn(Q { z: mkz(8), t: 8 })}");
    let s = N { v: 9 }; println(f"i{pn(P { a: s, b: N { v: 10 } })}");
    let s2 = N { v: 11 }; println(f"j{pn(P { a: s2, b: s2 })}"); println(f"j2{s2.v}");
    println(f"k{wn(mkw(12))}");
    println(f"l{wn(W { h: N { v: 13 }, k: 13 })}");
    mkz(14); println("m");
    mkw(15); println("n");
    mky(16); println("o");
    let _ = mkz(17); println("p");
    let _ = mkw(18); println("q");
    let _ = mky(19); println("r");
    let _ = Y { h: N { v: 20 }, k: 20 }; println("s");
    println("end");
}"#);
    assert_eq!(out, "dD1n1\na1\ndN1\ndD2n2\nb2\ndN2\nc3\ndD3n3\ndN3\nd4\ndN4\ndD5n5\ne5\ndN5\nf6\ndD6n6\ndN6\ng1\ndD7n7\ndD8n8\nh8\ndN8\ni19\ndN10\ndN9\nj22\nj211\ndN11\ndW12\ndN12\nk12\ndW13\ndN13\nl13\ndD14n14\ndN14\nm\ndW15\ndN15\nn\ndN16\no\ndD17n17\ndN17\np\ndW18\ndN18\nq\ndN19\nr\ndN20\ns\nend\ndN7\n");
}
