//! B-2026-09-29-107 -- a heap leaf destructured out of a by-value tuple param
//! or a loop element, then moved on, has exactly one owner.

use super::*;

/// B-2026-09-29-107 — the PARAM half. A `Vec` / `String` leaf of a by-value
/// tuple param was an untracked alias of the callee's entry copy, so a whole
/// rebind (`let b = a`) gave the buffer a second owner and the copy's own drop
/// freed it again (crash on every compiled surface, 30 valgrind errors on this
/// program at -O0 before the fix). The leaf now takes the MEMORY and the
/// caller keeps the bodies; a rebind of that leaf, or of a bare `Vec` param
/// (`let b = v`, which ran each body in the callee and again in the caller),
/// is a view and arms no walker of its own.
#[test]
fn e2e_tuple_and_vec_param_heap_leaf_rebind_one_owner() {
    let src = r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
fn take(v: Vec[D]) -> i64 { return v.len() }
fn pv(t: (Vec[D], i64)) -> i64 { let (a, j) = t; let b = a; return b.len() + j }
fn pi(t: (Vec[i64], i64)) -> i64 { let (a, j) = t; let b = a; return b.len() + j }
fn ps(t: (String, i64)) -> i64 { let (a, j) = t; let b = a; return b.len() + j }
fn pm(t: (Vec[D], i64)) -> i64 { let (a, j) = t; let b = a; let c = b; return take(c) + j }
fn pr(t: (Vec[D], i64)) -> Vec[D] { let (a, j) = t; let b = a; println(f"j{j}"); return b }
fn vv(v: Vec[D]) -> i64 { let b = v; return b.len() }
fn vk(v: Vec[D], k: bool) -> i64 { let b = v; if k { return take(b) } return 0 }
fn main() {
    { let t: (Vec[D], i64) = ([mkd(1), mkd(2)], 7); println(f"a {pv(t)}") }
    println(f"b {pi(([1, 2, 3], 7))}")
    println(f"c {ps((f"a-heap-string-longer-than-sso-{1}", 7))}")
    { let t: (Vec[D], i64) = ([mkd(3)], 1); println(f"d {pm(t)}") }
    { let r = pr(([mkd(4)], 5)); println(f"e {r.len()}") }
    { let v: Vec[D] = [mkd(5), mkd(6)]; println(f"f {vv(v)}") }
    { let v: Vec[D] = [mkd(7)]; println(f"g {vk(v, true)}") let u: Vec[D] = [mkd(8)]; println(f"h {vk(u, false)}") }
    { let mut i = 0; while i < 3 { let t: (Vec[D], i64) = ([mkd(10 + i)], i); println(f"l {pv(t)}"); i = i + 1; } }
    println("end")
}
"#;
    let want = "a 9\ndD1\ndD2\nb 10\nc 38\nd 2\ndD3\nj5\ne 1\ndD4\nf 2\ndD5\ndD6\ng 1\ndD7\nh 0\ndD8\nl 1\ndD10\nl 2\ndD11\nl 3\ndD12\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-29-107 — the LOOP half. A loop's tuple element is a bit-copy view
/// of a slot the collection still owns, but its destructured leaves took the
/// element's memory and cap-zeroed only the view, and a `for (a, j) in v`
/// pattern never marked its leaves as loop borrows at all, so moving a leaf on
/// (`let b = a`, `out.push(a)`) double-freed on every compiled surface.
#[test]
fn e2e_loop_element_heap_leaf_moved_on_one_owner() {
    let src = r#"struct S { x: String }
fn main() {
    let v: Vec[(Vec[i64], i64)] = [([1], 1), ([2, 3], 2)];
    for pair in v.into_iter() { let (a, j) = pair; let b = a; println(f"a {j} {b.len()}") }
    let w: Vec[(String, i64)] = [(f"a-heap-string-longer-than-sso-{1}", 1)];
    for pair in w.into_iter() { let (a, j) = pair; let b = a; println(f"b {j} {b.len()}") }
    let x: Vec[(String, i64)] = [(f"a-heap-string-longer-than-sso-{2}", 2)];
    for (a, j) in x { let b = a; println(f"c {j} {b.len()}") }
    let y: Vec[(String, i64, String)] = [(f"a-heap-string-longer-than-sso-{3}", 3, f"b-heap-string-longer-than-sso-{3}")];
    for (a, j, c) in y { let b = a; let e = c; println(f"d {j} {b.len()} {e.len()}") }
    let z: Vec[(Vec[i64], i64)] = [([1], 1), ([2, 3], 2)];
    let mut out: Vec[Vec[i64]] = [];
    for pair in z.into_iter() { let (a, j) = pair; out.push(a) }
    println(f"e {out.len()} {out[1].len()}")
    let s: Vec[(S, i64)] = [(S { x: f"a-heap-string-longer-than-sso-{4}" }, 4)];
    for pair in s.into_iter() { let (q, j) = pair; let t = q; println(f"f {t.x.len()} {j}") }
    println("end")
}
"#;
    let want = "a 1 1\na 2 2\nb 1 31\nc 2 31\nd 3 31 31\ne 2 2\nf 31 4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
