//! B-2026-09-28-8 -- a boxed generic-enum param rebound through a generic
//! passthrough inside a generic middle function has one owner on every path.

use super::*;

/// B-2026-09-28-8 — the `Drop` payload spelling. Before: ASAN
/// `attempting double-free` (the inner monomorph and `main` both freed).
#[test]
fn asan_boxed_generic_enum_forwarded_through_passthrough_local_frees_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
enum G[T] { Y(T), N }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn glen[T](g: G[T]) -> i64 { return 1 }
fn gid[T](g: G[T]) -> G[T] { return g }
fn gvia[T](g: G[T]) -> i64 { let h = gid(g); return glen(h) }
fn gviam[T](g: G[T]) -> i64 { let h = gid(g); match h { G.Y(x) => { return 5 }, G.N => { return 0 } } }
fn gviac[T](g: G[T], c: bool) -> i64 { let h = gid(g); if c { return glen(h) } println("keep"); return 0 }
fn gviar[T](g: G[T]) -> G[T] { let h = gid(g); return h }
fn gvia2[T](g: G[T]) -> i64 { let h = gid(g); let k = gid(h); return glen(k) }
fn gviaf[T](g: G[T]) -> i64 { let h = gid(g); return gvia(h) }
fn gkeep[T](g: G[T]) -> i64 { let h = gid(g); println("keep"); return 0 }
fn main() {
    let a1: G[R] = G.Y(mk(1));
    println(f"q1 {gvia(a1)}");
    let a2: G[R] = G.Y(mk(2));
    println(f"q2 {gviam(a2)}");
    let a3: G[R] = G.Y(mk(3));
    println(f"q3 {gviac(a3, true)}");
    let a4: G[R] = G.Y(mk(4));
    println(f"q4 {gviac(a4, false)}");
    let a5: G[R] = G.Y(mk(5));
    let b5 = gviar(a5);
    println(f"q5 {glen(b5)}");
    let a6: G[R] = G.Y(mk(6));
    println(f"q6 {gvia2(a6)}");
    let a7: G[R] = G.Y(mk(7));
    println(f"q7 {gviaf(a7)}");
    let a8: G[R] = G.Y(mk(8));
    println(f"q8 {gkeep(a8)}");
    println("end")
}
"#,
        &[
            "d1", "q1 1", "d2", "q2 5", "d3", "q3 1", "d4", "keep", "q4 0", "d5", "q5 1", "d6",
            "q6 1", "d7", "q7 1", "d8", "keep", "q8 0", "end",
        ],
        "asan_boxed_generic_enum_forwarded_through_passthrough_local_frees_once",
    );
}

/// B-2026-09-28-8 — heap payloads, including `Array[String, 2]`, whose
/// hand-back at the passthrough needed the caller's propagated `T` resolved.
#[test]
fn asan_boxed_generic_enum_heap_payload_through_passthrough_local_frees_once() {
    assert_clean_asan_run(
        r#"enum G[T] { Y(T), N }
fn glen[T](g: G[T]) -> i64 { return 1 }
fn gid[T](g: G[T]) -> G[T] { return g }
fn gvia[T](g: G[T]) -> i64 { let h = gid(g); return glen(h) }
fn gviac[T](g: G[T], c: bool) -> i64 { let h = gid(g); if c { return glen(h) } println("keep"); return 0 }
fn gvia2[T](g: G[T]) -> i64 { let h = gid(g); let k = gid(h); return glen(k) }
fn main() {
    let s: G[String] = G.Y(f"ffffffffffff51-heap-string-longer");
    println(f"s {gvia(s)}");
    let a: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqq4", f"rrrrrrrrrrrrrrrrrrrrrrrrrrrr5"]);
    println(f"a {gvia(a)}");
    let c1: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqqq8", f"rrrrrrrrrrrrrrrrrrrrrrrrrrrr9"]);
    println(f"c1 {gviac(c1, false)}");
    let c2: G[Array[String, 2]] = G.Y([f"qqqqqqqqqqqqqqqqqqqqqqqqqqqq10", f"rrrrrrrrrrrrrrrrrrrrrrrrrrr11"]);
    println(f"c2 {gviac(c2, true)}");
    let v0: Vec[String] = [f"aaaaaaaaaaaaaaaaaaaaaaaaaaaaa1", f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbb2"];
    let v: G[Vec[String]] = G.Y(v0);
    println(f"v {gvia2(v)}");
    println("end")
}
"#,
        &["s 1", "a 1", "keep", "c1 0", "c2 1", "v 1", "end"],
        "asan_boxed_generic_enum_heap_payload_through_passthrough_local_frees_once",
    );
}
