//! B-2026-09-26-38 -- a generic enum's multi-field variant owns the heap its
//! erased field boxes, whatever that field instantiates to.

use super::*;

/// B-2026-09-26-38 — `enum G2[T] { X(T, i64), Y }` boxed a `T` that outgrew
/// its one-word slot, and only an ARRAY instantiation had an owner for that
/// box, so a `String`, `Vec` or struct field leaked the box and its contents
/// at `-O0` on every compiled surface (29 valgrind errors on this program
/// before the fix). Covered: `String` / `Vec[i64]` / `Vec[R]` / struct
/// instantiations, the erased field second or twice, read-only, wildcard and
/// consuming arms, a by-value callee that matches, a loop, a `Vec` of them,
/// a rebind, a reassignment, a returned value, `if let`, and the two-param
/// `X2[A, B]` spelling of B-2026-09-28-57.
#[test]
fn e2e_generic_multi_field_enum_heap_field_owned() {
    let src = r#"struct R { id: i64 }
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
"#;
    let want = "c1\ndR1\ndR2\nc2\nc3\nc4\nc5\nc6 a-heap-string-longer-than-sso-6 6\nc7 7\nc8 a-heap-string-longer-than-sso-8 8\nc9 40\ndR0\ndR1\ndR2\nc10\nc11 3\nc13 a-heap-string-longer-than-sso-13 5\nc14 a-heap-string-longer-than-sso-14 5\nc15 b-heap-string-longer-than-sso-15 6\nc19 3\ndR19\ndR20\ndR21\nc20 a-heap-string-longer-than-sso-20 9\nc21 2 5\ndR22\ndR23\nc22 3 5\ndR24\ndR25\ndR26\ndW2\nm1\ndW3\nm7\nrd a333 7\ndW4\nm4\nwc 7\ndW5\nm5\ndW6\nm9 7\ndW7\nm10\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-26-38 — the same enums handed BY VALUE to a callee whose arm
/// takes the payload. Before the fix the callee's `Vec[R]` binding freed its
/// buffer and the param's element walker then read it (ASLR-varying ids at
/// `-O2`), and once the box had an owner a two-binding arm (`X2.Two(r, n)`)
/// ran the rebound payload's `Drop` body on no compiled surface. Compared as
/// a SET: the compiled backends run these bodies inside the callee, which is
/// B-2026-09-27-108's open ordering question, and `--interp` loses the `dW`
/// bodies here (a separate row), so neither order nor the interpreter is the
/// oracle.
#[test]
fn e2e_generic_multi_field_enum_param_arm_bodies_once() {
    let src = r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { a: String, b: String, c: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}") } }
fn mkw(n: i64) -> W { return W { a: f"a{n}", b: f"bbb{n}", c: f"ccc{n}" } }
enum G2[T] { X(T, i64), Y }
enum X2[A, B] { Two(A, B), Nil }
fn gv(w: G2[Vec[R]]) -> i64 { match w { G2.X(v, n) => v.len() + n, G2.Y => 0 } }
fn cz(x: X2[W, i64]) -> i64 { match x { X2.Two(r, n) => { let z = r; n } X2.Nil => 0 } }
fn main() {
    let w: G2[Vec[R]] = G2.X([R { id: 1 }, R { id: 2 }], 31);
    println(f"k3 {gv(w)}");
    println(f"m8 {cz(X2.Two(mkw(77), 7))}");
    let q = X2.Two(mkw(4444), 8);
    println(f"m3 {cz(q)}");
    println("end")
}
"#;
    let Some(out) = run_program(src) else {
        return;
    };
    let mut got: Vec<&str> = out.lines().collect();
    got.sort_unstable();
    let mut want = vec!["k3 33", "dR1", "dR2", "m8 7", "dW3", "m3 8", "dW5", "end"];
    want.sort_unstable();
    assert_eq!(got, want, "AOT");
}
