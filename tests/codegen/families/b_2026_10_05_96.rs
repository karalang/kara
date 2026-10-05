//! B-2026-10-05-96: a generic struct literal nested in another one

use super::*;

/// B-2026-10-05-96: `W { inner: Bx { v: 5, n: 0 }, tag: 3 }`, with
/// `inner: Bx[T]`, was refused on every surface ("cannot infer type parameter
/// 'T'"): the inner literal was seeded with `W`'s own `T` and typed as
/// `Bx[T]`. Each literal here binds its parameters from the values, including
/// two parameters bound by two nested literals and a `T` that is itself a
/// generic struct.
#[test]
fn e2e_nested_generic_struct_literal_binds_from_values() {
    let Some(out) = run_program(
        r#"struct Bx[T] { v: T, n: i64 }
struct W[T] { inner: Bx[T], tag: T }
struct Pr[A, B] { a: Bx[A], b: Bx[B] }
fn mk[T](x: T, y: T) -> W[T] { W { inner: Bx { v: x, n: 1 }, tag: y } }
fn main() {
    let w = W { inner: Bx { v: 5, n: 0 }, tag: 3 };
    println(f"{w.tag} {w.inner.v} {w.inner.n}");
    let s = W { inner: Bx { v: "ab", n: 2 }, tag: "cd" };
    println(f"{s.tag}{s.inner.v} {s.inner.n}");
    let p = Pr { a: Bx { v: 1, n: 2 }, b: Bx { v: "x", n: 3 } };
    println(f"{p.a.v} {p.b.v} {p.a.n + p.b.n}");
    let m = mk("q", "r");
    println(f"{m.inner.v}{m.tag} {m.inner.n}");
    let ww = W { inner: Bx { v: Bx { v: 6, n: 0 }, n: 0 }, tag: Bx { v: 1, n: 1 } };
    println(f"{ww.inner.v.v} {ww.tag.n}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "3 5 0\ncdab 2\n1 x 5\nqr 1\n6 1\n");
}
