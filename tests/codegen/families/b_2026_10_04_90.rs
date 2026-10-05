//! B-2026-10-04-90 -- a method call on a container held in a tuple that is
//! itself an indexed element or a map value (`mm[1].0.push(p)`,
//! `a[0].0.insert(1, p)`) failed the build with "no handler for method ...
//! on this tuple-element receiver" while `--interp` ran it.

use super::*;

/// The method acts on the container's own element: `push` / `len` / index
/// on a `Vec` in a map value's tuple, `insert` / `len` / `contains_key`
/// on a `Map` in an `Array` element's tuple, `push` on a `Vec` in a
/// `Vec` element's tuple, and `push` on a `Vec` in a tuple in a map that is
/// itself a `Vec` element.
#[test]
fn e2e_method_on_container_in_indexed_tuple() {
    let src = r#"struct P { n: i64, s: String }
fn mk(n: i64) -> P { P { n: n, s: f"p{n}" } }
fn main() {
    let mut mm: Map[i64, (Vec[P], i64)] = Map.new();
    mm.insert(1, (Vec.new(), 1));
    mm[1].0.push(mk(5));
    mm[1].0.push(mk(6));
    println(f"a:{mm[1].0.len()} {mm[1].0[1].s} {mm[1].1}");
    let mut a: Array[(Map[i64, P], i64), 1] = [(Map.new(), 1)];
    a[0].0.insert(1, mk(7));
    a[0].0.insert(2, mk(8));
    println(f"b:{a[0].0.len()} {a[0].0[2].s} {a[0].0.contains_key(1)}");
    let mut v: Vec[(Vec[String], i64)] = [([f"x{1}"], 2)];
    v[0].0.push(f"y{2}");
    println(f"c:{v[0].0.len()} {v[0].0[1]}");
    let mut vm: Vec[Map[i64, (Vec[i64], String)]] = Vec.new();
    let mut m2: Map[i64, (Vec[i64], String)] = Map.new();
    m2.insert(3, ([1, 2], f"z{3}"));
    vm.push(m2);
    vm[0][3].0.push(9);
    println(f"d:{vm[0][3].0.len()} {vm[0][3].0[2]} {vm[0][3].1.len()}");
}
"#;
    let want = "a:2 p6 1\nb:2 p8 true\nc:2 y2\nd:3 9 2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
