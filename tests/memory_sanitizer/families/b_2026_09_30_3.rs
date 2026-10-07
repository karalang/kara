//! B-2026-09-30-3 -- a tuple pattern matched against a borrowed `for` element
//! frees each heap leaf once.

use super::*;

/// B-2026-09-30-3 — `match` / `if let` / `while let` with a TUPLE pattern
/// over a `for` loop's tuple element. The element is a bit-copy of the
/// container's slot, but only its `Vec` / `String` siblings were classed as
/// borrowed bindings, so every heap leaf the arm bound registered a free of
/// its own and the container freed it again: a double free on every compiled
/// surface (a segfault once the leaf's elements ran `Drop` bodies). The leaves
/// are views now and a consuming use of one (`out.push(a)`, `let b = a`,
/// `take(a)`) takes a copy.
#[test]
fn asan_tuple_pattern_match_on_loop_element_frees_once() {
    assert_clean_asan_run(
        r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
fn take(x: Vec[i64]) -> i64 { return x.len() }
fn main() {
    let v: Vec[(Vec[i64], i64)] = [([1], 1), ([2, 3], 2)];
    for pair in v.iter() { match pair { (a, j) => println(f"a {j} {a.len()}") } }
    for pair in v { match pair { (a, j) => println(f"b {j} {a.len()}") } }
    for pair in v.iter() { match pair { (a, 1) => println(f"c one {a.len()}"), _ => println("c other") } }
    for pair in v.iter() { if let (a, 1) = pair { println(f"d {a.len()}") } }
    for pair in v.iter() { while let (a, 2) = pair { println(f"e {a.len()}"); break } }
    for pair in v.iter() { match pair { (a, j) => { let b = a; println(f"f {j} {b.len()}") } } }
    for pair in v.iter() { match pair { (a, j) => println(f"g {take(a)}") } }
    let mut out: Vec[Vec[i64]] = Vec.new();
    for pair in v.iter() { match pair { (a, j) => out.push(a) } }
    println(f"h {v.len()} {out.len()} {out[1].len()}");
    let w: Vec[(String, i64)] = [(f"a-heap-string-longer-than-sso-{1}", 1)];
    for (i, pair) in w.iter().enumerate() { match pair { (a, j) => println(f"i {i} {a.len()}") } }
    let n: Vec[((String, i64), i64)] = [((f"a-heap-string-longer-than-sso-{2}", 3), 4)];
    for pair in n.iter() { match pair { ((a, k), j) => println(f"j {a.len()} {k} {j}") } }
    { let dv: Vec[(Vec[D], i64)] = [([mkd(1)], 1), ([mkd(2)], 2)]; for pair in dv.iter() { match pair { (a, j) => println(f"k {j} {a.len()}") } } println(f"k{dv.len()}") }
    println("end")
}
"#,
        &[
            "a 1 1", "a 2 2", "b 1 1", "b 2 2", "c one 1", "c other", "d 1", "e 2", "f 1 1",
            "f 2 2", "g 1", "g 2", "h 2 2 2", "i 0 31", "j 31 3 4", "k 1 1", "k 2 1", "k2", "dD1",
            "dD2", "end",
        ],
        "B-2026-09-30-3 tuple pattern match on loop element",
    );
}
