//! B-2026-10-04-90 -- a method on a container held in an indexed tuple acts
//! in place: no copy of the container is made, and none is leaked.

use super::*;

/// Every element pushed or inserted is freed once by its container. The
/// `len()` on a map in an `Array` element's tuple used to deep-clone the map
/// in the len family's value arm and leak the clone (72 bytes) before the
/// tuple-element path answered.
#[test]
fn asan_method_on_container_in_indexed_tuple() {
    assert_clean_asan_run(
        r#"struct P { n: i64, s: String }
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
"#,
        &["a:2 p6 1", "b:2 p8 true", "c:2 y2", "d:3 9 2"],
        "method_on_container_in_indexed_tuple",
    );
}
