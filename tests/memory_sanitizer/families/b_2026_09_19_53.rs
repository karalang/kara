//! B-2026-09-19-53 -- a `shared enum`'s heap-boxed payload has exactly one
//! owner: the object's release, or the arm binding that takes it.

use super::*;

/// B-2026-09-19-53 — a generic `shared enum G[T]`'s erased `T` payload is
/// heap-boxed, and before the fix nothing freed that box: a read-only arm
/// leaked it, and an arm that handed the payload on double-freed it (the
/// `Array` hand-on aborted, the `Vec` and `if let` legs did once the box had an
/// owner). Covers `match` / `if let` / `while let` / `let ... else`, named,
/// parameter and temporary scrutinees, and arms that read, hand on or return.
#[test]
fn asan_shared_generic_enum_boxed_payload_one_owner() {
    assert_clean_asan_run(
        r#"shared enum G[T] { Y(T), N }
struct P { s: String, n: i64 }
fn eat(a: Array[String, 2]) -> i64 { return a[0].len() }
fn eatv(v: Vec[String]) -> i64 { return v.len() }
fn eats(s: String) -> i64 { return s.len() }
fn eatp(p: P) -> i64 { return p.s.len() + p.n }
fn mka(s: String) -> Array[String, 2] { return [s, f"yy"] }
fn mkv(s: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(s); return v }
fn rda(g: G[Array[String, 2]]) -> i64 { match g { G.Y(x) => { x[0].len() } G.N => { 0 } } }
fn hov(g: G[Vec[String]]) -> i64 { match g { G.Y(x) => { eatv(x) } G.N => { 0 } } }
fn hoa(g: G[Array[String, 2]]) -> i64 { match g { G.Y(x) => { eat(x) } G.N => { 0 } } }
fn hos(g: G[String]) -> i64 { match g { G.Y(x) => { eats(x) } G.N => { 0 } } }
fn hop(g: G[P]) -> i64 { match g { G.Y(x) => { eatp(x) } G.N => { 0 } } }
fn ta(g: G[Array[String, 2]]) -> Array[String, 2] { match g { G.Y(x) => { return x } G.N => { return [f"n", f"n"] } } }
fn tv(g: G[Vec[String]]) -> Vec[String] { match g { G.Y(x) => { return x } G.N => { return Vec.new() } } }
fn le(g: G[Vec[String]]) -> i64 { let G.Y(x) = g else { return 0 }; return eatv(x) }
fn main() {
    let mut i = 0;
    while i < 2 {
        println(f"r{rda(G.Y(mka(f"aaa{i}")))} v{hov(G.Y(mkv(f"b{i}")))} a{hoa(G.Y(mka(f"cc{i}")))} s{hos(G.Y(f"dddd{i}"))} p{hop(G.Y(P { s: f"e{i}", n: 5 }))}");
        let a = ta(G.Y(mka(f"ff{i}"))); let v = tv(G.Y(mkv(f"gg{i}"))); println(f"t{a[0]} {v[0]} l{le(G.Y(mkv(f"h{i}")))}");
        i = i + 1;
    }
    { let a: Array[String, 2] = [f"x1", f"y1"]; let g: G[Array[String, 2]] = G.Y(a); match g { G.Y(x) => { println(f"m{eat(x)}") } G.N => {} }; println("c1") }
    { let a: Array[String, 2] = [f"x2", f"y2"]; let g: G[Array[String, 2]] = G.Y(a); if let G.Y(x) = g { println(f"i{eat(x)}") }; println("c2") }
    { let g: G[Vec[String]] = G.Y(mkv(f"x3")); if let G.Y(x) = g { println(f"u{eatv(x)}") }; println("c3") }
    { match G.Y(mka(f"x4")) { G.Y(x) => { println(f"k{eat(x)}") } G.N => {} }; println("c4") }
    { let a: Array[String, 2] = [f"x5", f"y5"]; let g: G[Array[String, 2]] = G.Y(a); match g { G.Y(x) => { println(f"o{x[1]}") } G.N => {} }; println("c5") }
    { if let G.Y(x) = G.Y(mka(f"x6")) { println(f"j{eat(x)}") }; println("c6") }
    { if let G.Y(x) = G.Y(mkv(f"x7")) { println(f"q{eatv(x)}") }; println("c7") }
    { let mut n = 0; let g: G[Vec[String]] = G.Y(mkv(f"x8")); while let G.Y(x) = g { n = n + x.len(); if n > 0 { break } }; println(f"w{n}") }
    println("end")
}
"#,
        &[
            "r4 v1 a3 s5 p7",
            "tff0 gg0 l1",
            "r4 v1 a3 s5 p7",
            "tff1 gg1 l1",
            "m2",
            "c1",
            "i2",
            "c2",
            "u1",
            "c3",
            "k2",
            "c4",
            "oy5",
            "c5",
            "j2",
            "c6",
            "q1",
            "c7",
            "w1",
            "end",
        ],
        "asan_shared_generic_enum_boxed_payload_one_owner",
    );
}

/// B-2026-09-19-53 — the `if let` / `while let` / `let ... else` legs never ran
/// the `match` arm's shared-enum move-out disarm, so a `Vec` bound out of a
/// MONOMORPHIC `shared enum` was freed by the binding and again by the box:
/// two invalid frees per cell, read-only arm included.
#[test]
fn asan_shared_enum_let_pattern_vec_payload_one_owner() {
    assert_clean_asan_run(
        r#"shared enum M { Y(Vec[String]), N }
fn eatv(v: Vec[String]) -> i64 { return v.len() }
fn mkv(s: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(s); return v }
fn main() {
    { let g: M = M.Y(mkv(f"q1")); if let M.Y(x) = g { println(f"v{eatv(x)}") }; println("c1") }
    { let g: M = M.Y(mkv(f"q2")); if let M.Y(x) = g { println(f"r{x.len()}") }; println("c2") }
    { let mut n = 0; let g: M = M.Y(mkv(f"q3")); while let M.Y(x) = g { n = n + x.len(); if n > 0 { break } }; println(f"w{n}") }
    { let g: M = M.Y(mkv(f"q4")); let M.Y(x) = g else { return }; println(f"e{eatv(x)}"); println("c4") }
    println("end")
}
"#,
        &["v1", "c1", "r1", "c2", "w1", "e1", "c4", "end"],
        "asan_shared_enum_let_pattern_vec_payload_one_owner",
    );
}
