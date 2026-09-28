//! B-2026-09-28-8 -- a boxed generic-enum param rebound through a generic
//! PASSTHROUGH (`let h = gid(g)`) inside a generic middle function has one
//! owner, and its payload's `Drop` body runs once, whatever the rebound local
//! does next.

use super::*;

/// B-2026-09-28-8 — a param whose only escape is `let h = gid(g)` with `h`
/// itself only taken counts as taken, so the caller stands down and the
/// middle owns the box; the rebound local is no caller VIEW, so it carries
/// the bodies. Before: every cell that forwards `h` aborted with a double
/// free, and `gviam` / `gviac(.., false)` / `gkeep` ran no body. `gviar`
/// hands `h` back and stays caller-owned; `gvia2` is a two-link chain.
#[test]
fn e2e_boxed_generic_enum_forwarded_through_passthrough_local_has_one_owner() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "d1\nq1 1\nd2\nq2 5\nd3\nq3 1\nd4\nkeep\nq4 0\nd5\nq5 1\nd6\nq6 1\nd7\nq7 1\nd8\nkeep\nq8 0\nend\n", "got:\n{out}");
}
