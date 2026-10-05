//! B-2026-10-05-2 -- a tuple element cloned through an expression subscript is
//! a deep copy: the original and the clone are each freed once.

use super::*;

/// The clone owns its own buffer, so the container and the clone each free
/// theirs once, and the subscript expression is compiled once.
#[test]
fn asan_clone_tuple_element_at_expression_subscript() {
    assert_clean_asan_run(
        r#"fn idx(k: mut ref i64) -> i64 { k += 1; return k; }
fn main() {
    let cases: Vec[(String, i64)] = [("ab".to_string(), 1), ("cd".to_string(), 2)];
    let i = 0;
    let u = cases[i + 1].0.clone();
    println(f"a:{u} {cases[i].0}");
    let vv: Vec[(Vec[i64], String)] = [([1, 2], f"x{1}"), ([3], f"y{2}")];
    let w = vv[i + 1].0.clone();
    let s = vv[i * 1].1.clone();
    println(f"b:{w.len()} {w[0]} {s}");
    let mut k = 0;
    let z = cases[idx(mut k) - 1].0.clone();
    println(f"c:{z} k={k}");
    let ar: Array[(String, i64), 2] = [(f"p{1}", 1), (f"q{2}", 2)];
    let t = ar[i + 1].0.clone();
    println(f"d:{t}");
    let mut m: Map[i64, (String, i64)] = Map.new();
    m.insert(2, (f"m{2}", 5));
    let mv = m[i + 2].0.clone();
    println(f"e:{mv}");
}
"#,
        &["a:cd ab", "b:1 3 x1", "c:ab k=1", "d:q2", "e:m2"],
        "clone_tuple_element_at_expression_subscript",
    );
}
