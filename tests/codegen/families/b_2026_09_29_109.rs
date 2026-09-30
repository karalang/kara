//! B-2026-09-29-109 -- a leaf destructured out of a `for` element the
//! collection still owns is a view on `--interp` too.

use super::*;

/// B-2026-09-29-109 — `for pair in v.iter() { let (a, j) = pair; .. }`: the
/// loop borrows the element and `v` runs its bodies at its own death, but the
/// interpreter gave the destructured leaves drop slots, so each body ran at
/// the leaf's death instead (and a second time for a `D` leaf, whose element
/// `v` still walked). With the leaf a view, the `Vec` binding's walk also has
/// to reach a tuple element's inner `Vec`, which it had skipped to avoid
/// that double: `let v: Vec[(Vec[D], i64)]` alone ran no body at all.
/// Covers a function frame, `let .. else`, a field-chain source, the
/// `enumerate` / `rev` adaptors, a self-rebind, a nested loop reusing the
/// name, and `.into_iter()`, which lends on every backend (B-2026-09-27-76).
#[test]
fn e2e_loop_borrowed_element_destructure_leaves_are_views() {
    let src = r#"struct D { id: i64, s: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}") } }
fn mkd(n: i64) -> D { return D { id: n, s: f"sssssssssssssssssssssssssssssssssss{n}" } }
struct W { d: D, k: i64 }
struct H { xs: Vec[(D, i64)] }
fn sum(v: Vec[(D, i64)]) -> i64 { let mut s = 0; for pair in v.iter() { let (a, j) = pair; s = s + a.id + j; } return s }
fn main() {
    { let v: Vec[(Vec[D], i64)] = [([mkd(1)], 1), ([mkd(2)], 2)]; for pair in v.iter() { let (a, j) = pair; println(f"a {j} {a.len()}") } println(f"a{v.len()}") }
    { let v: Vec[(Vec[D], i64)] = [([mkd(3)], 3)]; println(f"b{v.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(4), 4), (mkd(5), 5)]; for pair in v.iter() { let (a, j) = pair; println(f"c {j} {a.id}") } println(f"c{v.len()}") }
    { let v: Vec[W] = [W { d: mkd(6), k: 6 }]; for w in v.iter() { let W { d, k } = w; println(f"d {k} {d.id}") } println(f"d{v.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(7), 7), (mkd(8), 8)]; println(f"e {sum(v)}") }
    { let v: Vec[(D, i64)] = [(mkd(9), 1), (mkd(10), 2)]; for pair in v.iter() { let (a, 1) = pair else { continue }; println(f"f {a.id}") } println(f"f{v.len()}") }
    { let h = H { xs: [(mkd(11), 11)] }; for pair in h.xs.iter() { let (a, j) = pair; println(f"g {j} {a.id}") } println(f"g{h.xs.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(12), 12), (mkd(13), 13)]; for (i, pair) in v.iter().enumerate() { let (a, j) = pair; println(f"h {i} {a.id}") } for pair in v.iter().rev() { let (a, j) = pair; println(f"h {a.id}") } println(f"h{v.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(14), 14)]; for pair in v.iter() { let pair = pair; let (a, j) = pair; println(f"i {a.id}") } println(f"i{v.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(15), 15)]; let w: Vec[(D, i64)] = [(mkd(16), 16)]; for pair in v.iter() { for pair in w.iter() { let (a, j) = pair; println(f"j in {a.id}") } let (a, j) = pair; println(f"j out {a.id}") } println(f"j{v.len()}") }
    { let v: Vec[(D, i64)] = [(mkd(17), 17)]; for pair in v.into_iter() { let (a, j) = pair; println(f"k {a.id}") } println("k") }
    println("end")
}
"#;
    let want = "a 1 1\na 2 1\na2\ndD1\ndD2\nb1\ndD3\nc 4 4\nc 5 5\nc2\ndD4\ndD5\nd 6 6\nd1\ndD6\ne 30\ndD7\ndD8\nf 9\nf2\ndD9\ndD10\ng 11 11\ng1\ndD11\nh 0 12\nh 1 13\nh 13\nh 12\nh2\ndD12\ndD13\ni 14\ni1\ndD14\nj in 16\nj out 15\ndD16\nj1\ndD15\nk 17\ndD17\nk\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
