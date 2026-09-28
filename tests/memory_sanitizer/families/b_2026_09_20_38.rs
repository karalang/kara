//! B-2026-09-20-38 -- a boxed generic-enum payload FORWARDED through a generic
//! middle function has one owner on every path.

use super::*;

/// B-2026-09-20-38 — the `Drop` payload spelling. Before: a double free on
/// every forwarded cell (ASAN `attempting double-free`), the box freed by the
/// inner monomorph and again by `main`.
#[test]
fn asan_boxed_generic_enum_drop_payload_forwarded_through_generic_middle_frees_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
enum G[T] { Y(T), N }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn glen[T](g: G[T]) -> i64 { return 1 }
fn gm[T](g: G[T]) -> i64 { match g { G.Y(x) => { return 2 }, G.N => { return 0 } } }
fn gfwd[T](g: G[T]) -> i64 { return glen(g) }
fn gtop[T](g: G[T]) -> i64 { return gfwd(g) }
fn gcond[T](g: G[T], c: bool) -> i64 { if c { return glen(g) } println("keep"); return 0 }
fn gbr[T](g: G[T], c: bool) -> i64 { if c { return glen(g) } else { return gm(g) } }
fn gany[T](x: T) -> i64 { return 3 }
fn gfa[T](g: G[T]) -> i64 { return gany(g) }
fn gr[T](g: ref G[T]) -> i64 { return 4 }
fn gfr[T](g: G[T]) -> i64 { return gr(g) }
fn gpair[T](g: G[T], h: G[R]) -> i64 { return glen(g) + glen(h) }
fn main() {
    let g1: G[R] = G.Y(mk(1));
    println(f"q1 {gfwd(g1)}");
    let g2: G[R] = G.Y(mk(2));
    println(f"q2 {gtop(g2)}");
    let t3 = gfwd(G.Y(mk(3)));
    println(f"q3 {t3}");
    let g4: G[R] = G.Y(mk(4));
    println(f"q4 {gcond(g4, true)}");
    let g5: G[R] = G.Y(mk(5));
    println(f"q5 {gcond(g5, false)}");
    let g6: G[R] = G.Y(mk(6));
    println(f"q6 {gbr(g6, true)}");
    let g7: G[R] = G.Y(mk(7));
    println(f"q7 {gbr(g7, false)}");
    let g8: G[R] = G.Y(mk(8));
    println(f"q8 {gfa(g8)}");
    let g9: G[R] = G.Y(mk(9));
    println(f"q9 {gfr(g9)}");
    let g10: G[R] = G.Y(mk(10));
    let g11: G[R] = G.Y(mk(11));
    println(f"q10 {gpair(g10, g11)}");
    let g12: G[R] = G.N;
    println(f"q12 {gcond(g12, true)}");
    println("end")
}
"#,
        &[
            "d1", "q1 1", "d2", "q2 1", "d3", "q3 1", "d4", "q4 1", "keep", "d5", "q5 0", "d6",
            "q6 1", "d7", "q7 2", "q8 3", "d8", "q9 4", "d9", "d10", "d11", "q10 2", "q12 1",
            "end",
        ],
        "asan_boxed_generic_enum_drop_payload_forwarded_through_generic_middle_frees_once",
    );
}

/// B-2026-09-20-38 — the heap payloads the row measured (`String`,
/// `Vec[String]`, a struct, `Array[String, 2]`), two and three deep and
/// forwarded on one path only. The `Array` cells needed the inner call to
/// resolve the caller's PROPAGATED `T` (a nameless binding), which the call's
/// own type frame omits.
#[test]
fn asan_boxed_generic_enum_heap_payload_forwarded_through_generic_middle_frees_once() {
    assert_clean_asan_run(
        r#"struct Pr { a: String, b: i64 }
enum G[T] { Y(T), N }
fn glen[T](g: G[T]) -> i64 { return 1 }
fn gm[T](g: G[T]) -> i64 { match g { G.Y(x) => { return 2 }, G.N => { return 0 } } }
fn gfwd[T](g: G[T]) -> i64 { return glen(g) }
fn gtop[T](g: G[T]) -> i64 { return gfwd(g) }
fn gcond[T](g: G[T], c: bool) -> i64 { if c { return glen(g) } println("keep"); return 0 }
fn gbr[T](g: G[T], c: bool) -> i64 { if c { return glen(g) } else { return gm(g) } }
fn main() {
    let s: G[String] = G.Y(f"ffffffffffff51-heap-string-longer");
    println(f"s {gfwd(s)}");
    let v0: Vec[String] = [f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa1", f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbb2"];
    let v: G[Vec[String]] = G.Y(v0);
    println(f"v {gfwd(v)}");
    let p: G[Pr] = G.Y(Pr { a: f"ppppppppppppppppppppppppppppp3", b: 3 });
    println(f"p {gtop(p)}");
    let a: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqq4", f"rrrrrrrrrrrrrrrrrrrrrrrrrrrr5"]);
    println(f"a {gfwd(a)}");
    let a3: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqq6", f"rrrrrrrrrrrrrrrrrrrrrrrrrrrr7"]);
    println(f"a3 {gtop(a3)}");
    let c1: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqq8", f"rrrrrrrrrrrrrrrrrrrrrrrrrrrr9"]);
    println(f"c1 {gcond(c1, false)}");
    let c2: G[String] = G.Y(f"ffffffffffff52-heap-string-longer");
    println(f"c2 {gbr(c2, false)}");
    println("end")
}
"#,
        &[
            "s 1", "v 1", "p 1", "a 1", "a3 1", "keep", "c1 0", "c2 2", "end",
        ],
        "asan_boxed_generic_enum_heap_payload_forwarded_through_generic_middle_frees_once",
    );
}
