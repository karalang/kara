//! B-2026-09-29-107 -- a heap leaf destructured out of a by-value tuple param
//! or a loop element, then moved on, is freed exactly once.

use super::*;

/// B-2026-09-29-107 — param half: a rebound `Vec` / `String` leaf of a
/// by-value tuple param, and a rebound bare `Vec` param.
#[test]
fn asan_tuple_and_vec_param_heap_leaf_rebind_frees_once() {
    assert_clean_asan_run(
        r#"struct D { id: i64, s: String }
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
    println(f"b {pi(([1, 2, 3], 7))}");
    println(f"c {ps((f"a-heap-string-longer-than-sso-{1}", 7))}");
    { let t: (Vec[D], i64) = ([mkd(3)], 1); println(f"d {pm(t)}") }
    { let r = pr(([mkd(4)], 5)); println(f"e {r.len()}") }
    { let v: Vec[D] = [mkd(5), mkd(6)]; println(f"f {vv(v)}") }
    { let v: Vec[D] = [mkd(7)]; println(f"g {vk(v, true)}"); let u: Vec[D] = [mkd(8)]; println(f"h {vk(u, false)}") }
    { let mut i = 0; while i < 3 { let t: (Vec[D], i64) = ([mkd(10 + i)], i); println(f"l {pv(t)}"); i = i + 1; } }
    println("end")
}
"#,
        &[
            "a 9", "dD1", "dD2", "b 10", "c 38", "d 2", "dD3", "j5", "e 1", "dD4", "f 2", "dD5",
            "dD6", "g 1", "dD7", "h 0", "dD8", "l 1", "dD10", "l 2", "dD11", "l 3", "dD12", "end",
        ],
        "B-2026-09-29-107 tuple and Vec param heap leaf rebind",
    );
}

/// B-2026-09-29-107 — loop half: leaves of a loop's tuple element moved on.
#[test]
fn asan_loop_element_heap_leaf_moved_on_frees_once() {
    assert_clean_asan_run(
        r#"struct S { x: String }
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
    println(f"e {out.len()} {out[1].len()}");
    let s: Vec[(S, i64)] = [(S { x: f"a-heap-string-longer-than-sso-{4}" }, 4)];
    for pair in s.into_iter() { let (q, j) = pair; let t = q; println(f"f {t.x.len()} {j}") }
    println("end")
}
"#,
        &[
            "a 1 1",
            "a 2 2",
            "b 1 31",
            "c 2 31",
            "d 3 31 31",
            "e 2 2",
            "f 31 4",
            "end",
        ],
        "B-2026-09-29-107 loop element heap leaf moved on",
    );
}
