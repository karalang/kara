//! B-2026-10-05-91: field reads on `self` in a concrete impl over a generic struct

use super::*;

/// B-2026-10-05-91: inside `impl Bx[i64]` the receiver was typed as the
/// erased `Bx`, so `self.v`, declared `v: T`, read as the bare `T` and
/// `self.v + self.n` was refused ("type 'T' does not implement trait Add"),
/// as was `self.v.len()` in `impl Bx[String]` and `self.a.clone()` in a
/// two-parameter impl. A write through `mut ref self` and a field of a nested
/// generic struct (`self.inner.v` in `impl W[i64]`) read the same way. Each method now operates on the field at the impl's
/// type, next to a generic `impl[T] Bx[T]` that must keep reading `T`.
#[test]
fn e2e_concrete_impl_reads_generic_fields_at_its_args() {
    let Some(out) = run_program(
        r#"struct Bx[T] { v: T, n: i64 }
impl[T] Bx[T] {
    fn count(ref self) -> i64 { return self.n; }
}
impl Bx[i64] {
    fn get(self) -> i64 { return self.v + self.n; }
    fn bump(mut ref self) { self.v = self.v + 1; self.v += self.n; }
}
struct W[T] { inner: Bx[T], tag: T }
impl W[i64] {
    fn f(ref self) -> i64 { return self.inner.v + self.tag; }
}
impl Bx[String] {
    fn len2(ref self) -> i64 { return self.v.len() + self.n; }
    fn twice(ref self) -> String { return self.v.clone() + self.v.clone(); }
}
struct P[A, B] { a: A, b: B }
impl P[String, Vec[i64]] {
    fn name(ref self) -> String { return self.a.clone(); }
    fn total(ref self) -> i64 {
        let mut t = 0;
        for x in self.b { t += x; }
        return t;
    }
}
fn main() {
    let a = Bx { v: 40, n: 2 };
    println(a.count());
    let mut m = Bx { v: 1, n: 10 };
    m.bump();
    println(m.v);
    let bi = Bx { v: 5, n: 0 };
    let w = W { inner: bi, tag: 3 };
    println(w.f());
    println(a.get());
    let s = Bx { v: "abc".to_string(), n: 10 };
    println(s.len2());
    println(s.twice());
    println(s.count());
    let p = P { a: "pair".to_string(), b: [1, 2, 3] };
    println(p.name());
    println(p.total());
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "2\n12\n8\n42\n13\nabcabc\n10\npair\n6\n");
}
