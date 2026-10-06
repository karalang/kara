//! B-2026-10-06-60 -- a `for` over a tuple's `Map` element freed the map as a
//! temporary and the tuple freed it again (a double free); a tuple holding a
//! `SortedMap` / `SortedSet`, or a `Set` named by a binding, leaked instead.

use super::*;

#[test]
fn asan_for_over_tuple_map_borrows_it() {
    assert_clean_asan_run(
        r#"struct P { s: String }
fn mkp() -> (Map[i64, P], Vec[i64]) {
    let mut m: Map[i64, P] = Map.new();
    m.insert(1, P { s: f"p{1}" });
    (m, vec![7])
}
fn hashed() {
    let mut m: Map[i64, String] = Map.new(); m.insert(2, f"b{2}"); m.insert(1, f"a{1}");
    let t = (m, 3);
    let mut n = 0;
    for (k, v) in t.0 { n = n + k + v.len(); }
    let u: (Map[i64, String], i64) = (Map.new(), 4);
    for (k, v) in u.0 { n = n + k; }
    println(f"a:{n} {t.1} {u.1}");
}
fn sorted() {
    let mut m: SortedMap[i64, String] = SortedMap.new(); m.insert(2, f"b{2}"); m.insert(1, f"a{1}");
    let t: (SortedMap[i64, String], i64) = (m, 3);
    for (k, v) in t.0 { println(f"b:{k}={v}"); }
    let mut m2: SortedMap[i64, String] = SortedMap.new(); m2.insert(9, f"z{9}");
    let t2 = (m2, 1);
    for (k, v) in t2.0 { println(f"c:{k}={v}"); }
}
fn sets() {
    let mut s: Set[String] = Set.new();
    s.insert(f"e{1}");
    let u = (s, 2);
    for e in u.0 { println(f"d:{e}"); }
    let mut ss: SortedSet[String] = SortedSet.new();
    ss.insert(f"f{1}");
    let w = (ss, 3);
    for e in w.0 { println(f"e:{e}"); }
    let w2 = w;
    println(f"f:{u.1} {w2.1}");
}
fn deep_and_repeated() {
    let t = mkp();
    let mut n = 0;
    let mut i = 0;
    while i < 2 { for (k, v) in t.0 { n = n + k + v.s.len(); } i = i + 1; }
    for (k, v) in t.0.iter() { n = n + k; }
    for (k, v) in mkp().0 { n = n + k; }
    println(f"g:{n} {t.1.len()}");
}
fn main() { hashed(); sorted(); sets(); deep_and_repeated(); }
"#,
        &[
            "a:7 3 4", "b:1=a1", "b:2=b2", "c:9=z9", "d:e1", "e:f1", "f:2 3", "g:8 1",
        ],
        "asan_for_over_tuple_map_borrows_it",
    );
}
