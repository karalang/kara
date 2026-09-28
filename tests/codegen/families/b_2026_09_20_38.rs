//! B-2026-09-20-38 -- a boxed generic-enum payload FORWARDED through a generic
//! middle function (`fn gfwd[T](g: G[T]) -> i64 { glen(g) }`) has one owner:
//! the middle function takes the box when its only escape is that forward.

use super::*;

/// B-2026-09-20-38 — the caller stands its box drop down and the middle
/// function's prologue picks it up for a param whose only escape is a forward
/// to a generic callee that takes the box; the forward then zeroes it, as the
/// caller's direct call does. Before: every compiled build aborted with a
/// double free (`glen` freed the box, `main` freed it again) and printed
/// nothing. `gcond` / `gbr` forward on one path only, so the middle keeps the
/// box on the other; `gfa` (a bare-`T` callee) and `gfr` (a `ref` callee) are
/// not takers and keep today's caller-retained route.
#[test]
fn e2e_boxed_generic_enum_forwarded_through_generic_middle_has_one_owner() {
    let Some(out) = run_program(
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
    ) else {
        return;
    };
    assert_eq!(out, "d1\nq1 1\nd2\nq2 1\nd3\nq3 1\nd4\nq4 1\nkeep\nd5\nq5 0\nd6\nq6 1\nd7\nq7 2\nq8 3\nd8\nq9 4\nd9\nd10\nd11\nq10 2\nq12 1\nend\n", "got:\n{out}");
}
