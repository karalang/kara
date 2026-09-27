//! B-2026-09-17-12 -- a read-only arm over a local of a heap-boxed GENERIC
//! enum is a view of the box: the enum's own `Drop` body runs before its
//! payload's, and a second read-only match does not free the payload again.

use super::*;

/// B-2026-09-17-12 — the memory face: no `Drop` anywhere, and a second
/// read-only match (or `if let`, or a loop) over one local freed the boxed
/// payload's interior again. Before: 34 allocs / 40 frees, 9 errors at -O0.
/// `moved_field` is the guard the other way: `let u = t.tag` moves a piece
/// out of the payload, so that arm stays the interior's owner.
#[test]
fn asan_boxed_generic_enum_readonly_arm_is_a_view() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
fn mk(i: i64) -> R { return R { id: i, tag: f"tttttttttttttttttttttttt{i}" }; }
enum G[T] { X(T), Y }
enum P[T] { A(T), B(T) }
fn twice() { let g: G[R] = G.X(mk(1)); match g { G.X(t) => { println(f"a{t.id}") } G.Y => { println("a0") } }; match g { G.X(t) => { println(f"a{t.tag.len()}") } G.Y => { println("a0") } } }
fn then_iflet() { let g: G[R] = G.X(mk(2)); match g { G.X(t) => { println("b1") } G.Y => { println("b0") } }; if let G.X(t) = g { println(f"b{t.tag.len()}") } }
fn vec_payload() { let g: G[Vec[String]] = G.X(["cccccccccccccccccccccccc", "dd"]); match g { G.X(v) => { println(f"c{v.len()}") } G.Y => { println("c0") } }; match g { G.X(v) => { println(f"c{v[0].len()}") } G.Y => { println("c0") } } }
fn str_payload() { let g: G[String] = G.X("eeeeeeeeeeeeeeeeeeeeeeee"); match g { G.X(s) => { println(f"d{s.len()}") } G.Y => { println("d0") } }; match g { G.X(s) => { println(f"d{s}") } G.Y => { println("d0") } } }
fn two_variants() { let g: P[R] = P.A(mk(3)); match g { P.A(t) => { println(f"e{t.id}") } P.B(t) => { println(f"eb{t.id}") } }; match g { P.A(t) => { println(f"e{t.tag.len()}") } P.B(t) => { println("eb") } } }
fn moved_field() { let g: G[R] = G.X(mk(4)); match g { G.X(t) => { let u = t.tag; println(f"f{u.len()}") } G.Y => { println("f0") } } }
fn looped() { let g: G[R] = G.X(mk(5)); for i in 0..3 { match g { G.X(t) => { println(f"g{t.id}") } G.Y => { println("g0") } } } }
fn main() {
    twice();
    then_iflet();
    vec_payload();
    str_payload();
    two_variants();
    moved_field();
    looped();
    println("end");
}
"#,
        &[
            "a1",
            "a25",
            "b1",
            "b25",
            "c2",
            "c24",
            "d24",
            "deeeeeeeeeeeeeeeeeeeeeeee",
            "e3",
            "e25",
            "f25",
            "g5",
            "g5",
            "g5",
            "end",
        ],
        "boxed-generic-enum-readonly-arm-is-a-view",
    );
}
