//! B-2026-10-04-88 -- a tuple holding a `Map` freed the handle but not the
//! values' heap (a struct value's `String` field, a `Vec[String]` value's
//! strings), and a tuple that also held a `Vec` / `String` leaked the whole
//! map.

use super::*;

#[test]
fn asan_tuple_held_map_frees_its_values() {
    assert_clean_asan_run(
        r#"struct P { s: String }
struct Q { n: i64, s: String }
fn mk() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a{1}", 1);
    m
}
fn eat(t: (Map[String, i64], Vec[i64])) -> i64 { t.1.len() }
fn struct_value() {
    let mut m: Map[i64, P] = Map.new();
    m.insert(1, P { s: f"k{1}" });
    let tm: (Map[i64, P], i64) = (m, 1);
    println(f"a:{tm.1}");
}
fn vec_element() {
    let mut m: Map[i64, Q] = Map.new();
    m.insert(1, Q { n: 1, s: f"k{1}" });
    let a: Vec[(Map[i64, Q], i64)] = [(m, 1)];
    println(f"b:{a[0].1}");
}
fn vec_of_strings_value() {
    let mut m: Map[String, Vec[String]] = Map.new();
    m.insert(f"k{1}", vec![f"x{1}", f"x{2}"]);
    let t = (m, 3);
    println(f"c:{t.1}");
}
fn beside_a_vec() {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a{1}", 1);
    let t = (m, vec![1]);
    let u: (Map[String, i64], Vec[i64]) = (mk(), vec![4, 5]);
    let w = (mk(), f"s{5}");
    println(f"d:{t.1.len()} {u.1.len()} {w.1}");
}
fn moved() {
    let t = (mk(), vec![1]);
    let u = t;
    let t2 = (mk(), vec![1, 2]);
    let n = eat(t2);
    let (a, b) = (mk(), vec![1, 2, 3]);
    let mut v: Vec[(Map[String, i64], Vec[i64])] = Vec.new();
    v.push((mk(), vec![4]));
    println(f"e:{u.1.len()} {n} {a.len()} {b.len()} {v.len()}");
}
fn main() { struct_value(); vec_element(); vec_of_strings_value(); beside_a_vec(); moved(); }
"#,
        &["a:1", "b:1", "c:3", "d:1 2 s5", "e:1 2 1 3 1"],
        "asan_tuple_held_map_frees_its_values",
    );
}
