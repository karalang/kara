//! B-2026-09-30-3 -- a tuple pattern matched against a borrowed `for` element
//! binds views of the container's slot.

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
fn e2e_tuple_pattern_match_on_loop_element_binds_views() {
    let src = r#"struct D { id: i64, s: String }
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
    println(f"h {v.len()} {out.len()} {out[1].len()}")
    let w: Vec[(String, i64)] = [(f"a-heap-string-longer-than-sso-{1}", 1)];
    for (i, pair) in w.iter().enumerate() { match pair { (a, j) => println(f"i {i} {a.len()}") } }
    let n: Vec[((String, i64), i64)] = [((f"a-heap-string-longer-than-sso-{2}", 3), 4)];
    for pair in n.iter() { match pair { ((a, k), j) => println(f"j {a.len()} {k} {j}") } }
    { let dv: Vec[(Vec[D], i64)] = [([mkd(1)], 1), ([mkd(2)], 2)]; for pair in dv.iter() { match pair { (a, j) => println(f"k {j} {a.len()}") } } println(f"k{dv.len()}") }
    println("end")
}
"#;
    let want = "a 1 1\na 2 2\nb 1 1\nb 2 2\nc one 1\nc other\nd 1\ne 2\nf 1 1\nf 2 2\ng 1\ng 2\nh 2 2 2\ni 0 31\nj 31 3 4\nk 1 1\nk 2 1\nk2\ndD1\ndD2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
