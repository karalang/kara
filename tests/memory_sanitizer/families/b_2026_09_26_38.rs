//! B-2026-09-26-38 -- a generic enum's multi-field variant frees the heap its
//! erased field boxes exactly once.

use super::*;

/// B-2026-09-26-38 — the box a generic multi-field variant made for a
/// non-array `T` had no owner (29 valgrind errors on this program at -O0
/// before the fix, all of them lost blocks).
#[test]
fn asan_generic_multi_field_enum_heap_field_owned() {
    assert_clean_asan_run(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { a: String, n: i64 }
struct W { a: String, b: String, c: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}") } }
fn mkw(n: i64) -> W { return W { a: f"a{n}", b: f"bbb{n}", c: f"ccc{n}" } }
enum G2[T] { X(T, i64), Y }
enum G3[T] { X(i64, T), Y }
enum Gt[T] { X(T, T), Y }
enum X2[A, B] { Two(A, B), Nil }
fn tk(g: G2[String]) -> i64 { match g { G2.X(s, n) => s.len() + n, G2.Y => 0 } }
fn mk2(n: i64) -> G2[Vec[R]] { return G2.X([R { id: n }], n) }
fn mks(s: String) -> G2[String] { return G2.X(s, 9) }
fn rd(x: X2[W, i64]) { match x { X2.Two(r, n) => println(f"rd {r.a} {n}"), X2.Nil => println("n") } }
fn wc(x: X2[W, i64]) { match x { X2.Two(_, n) => println(f"wc {n}"), X2.Nil => println("n") } }
fn hb(x: G2[W]) -> W { match x { G2.X(r, n) => r, G2.Y => mkw(1) } }
fn main() {
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-1", 5); println("c1") }
    { let w: G2[Vec[R]] = G2.X([R { id: 1 }, R { id: 2 }], 5); println("c2") }
    { let w: G2[Vec[i64]] = G2.X([1, 2, 3], 5); println("c3") }
    { let w: G3[String] = G3.X(5, f"a-heap-string-longer-than-sso-4"); println("c4") }
    { let w: Gt[String] = Gt.X(f"a-heap-string-longer-than-sso-5", f"b-heap-string-longer-than-sso-5"); println("c5") }
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-6", 6); match w { G2.X(s, n) => println(f"c6 {s} {n}"), G2.Y => println("y") } }
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-7", 7); match w { G2.X(_, n) => println(f"c7 {n}"), G2.Y => println("y") } }
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-8", 8); match w { G2.X(s, n) => { let t = s; println(f"c8 {t} {n}") } G2.Y => println("y") } }
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-9", 9); println(f"c9 {tk(w)}") }
    { let mut i = 0; while i < 3 { let w = mk2(i); i = i + 1; } println("c10") }
    { let v: Vec[G2[String]] = [G2.X(f"a-heap-string-longer-than-sso-11", 1), G2.Y, G2.X(f"a-heap-string-longer-than-sso-12", 3)]; println(f"c11 {v.len()}") }
    { let w: G2[P] = G2.X(P { a: f"a-heap-string-longer-than-sso-13", n: 2 }, 5); match w { G2.X(p, n) => println(f"c13 {p.a} {n}"), G2.Y => println("y") } }
    { let w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-14", 5); let u = w; match u { G2.X(s, n) => println(f"c14 {s} {n}"), G2.Y => println("y") } }
    { let mut w: G2[String] = G2.X(f"a-heap-string-longer-than-sso-15", 5); w = G2.X(f"b-heap-string-longer-than-sso-15", 6); match w { G2.X(s, n) => println(f"c15 {s} {n}"), G2.Y => println("y") } }
    { let v: Vec[G2[Vec[R]]] = [G2.X([R { id: 19 }], 1), G2.Y, G2.X([R { id: 20 }, R { id: 21 }], 3)]; println(f"c19 {v.len()}") }
    { let w = mks(f"a-heap-string-longer-than-sso-20"); match w { G2.X(s, n) => println(f"c20 {s} {n}"), G2.Y => println("y") } }
    { let w: G2[Vec[R]] = G2.X([R { id: 22 }, R { id: 23 }], 5); if let G2.X(v, n) = w { println(f"c21 {v.len()} {n}") } }
    { let w: G2[Vec[R]] = G2.X([R { id: 24 }, R { id: 25 }], 5); match w { G2.X(v, n) => { let mut u = v; u.push(R { id: 26 }); println(f"c22 {u.len()} {n}") } G2.Y => println("y") } }
    { let q = X2.Two(mkw(1), 7); println("m1") }
    { let q: X2[W, String] = X2.Two(mkw(22), f"s-heap-string-longer-than-sso"); println("m7") }
    { rd(X2.Two(mkw(333), 7)); println("m4") }
    { let q = X2.Two(mkw(4444), 7); wc(q); println("m5") }
    { let q: G2[W] = G2.X(mkw(55555), 7); match q { G2.X(r, n) => { let z = r; println(f"m9 {n}") } G2.Y => println("n") } }
    { let q: G2[W] = G2.X(mkw(666666), 7); let w = hb(q); println("m10") }
    println("end")
}
"#,
        &[
            "c1",
            "dR1",
            "dR2",
            "c2",
            "c3",
            "c4",
            "c5",
            "c6 a-heap-string-longer-than-sso-6 6",
            "c7 7",
            "c8 a-heap-string-longer-than-sso-8 8",
            "c9 40",
            "dR0",
            "dR1",
            "dR2",
            "c10",
            "c11 3",
            "c13 a-heap-string-longer-than-sso-13 5",
            "c14 a-heap-string-longer-than-sso-14 5",
            "c15 b-heap-string-longer-than-sso-15 6",
            "c19 3",
            "dR19",
            "dR20",
            "dR21",
            "c20 a-heap-string-longer-than-sso-20 9",
            "c21 2 5",
            "dR22",
            "dR23",
            "c22 3 5",
            "dR24",
            "dR25",
            "dR26",
            "dW2",
            "m1",
            "dW3",
            "m7",
            "rd a333 7",
            "dW4",
            "m4",
            "wc 7",
            "dW5",
            "m5",
            "dW6",
            "m9 7",
            "dW7",
            "m10",
            "end",
        ],
        "B-2026-09-26-38 generic multi-field enum heap field owned",
    );
}
