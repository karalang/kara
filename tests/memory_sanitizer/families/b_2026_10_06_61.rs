//! B-2026-10-06-61 -- a method on a map value reached through a tuple element
//! (`t.0[k].push(x)`) failed `karac build`; the fixed lowering mutates the
//! value in place, so nothing may leak or be freed twice.

use super::*;

#[test]
fn asan_method_on_map_value_through_tuple_element() {
    assert_clean_asan_run(
        r#"fn hashed() {
    let mut m: Map[String, Vec[String]] = Map.new();
    m.insert(f"k{1}", vec![f"x{1}"]);
    let mut t = (m, 3);
    t.0[f"k{1}"].push(f"y{2}");
    let mut i = 0;
    while i < 3 { t.0[f"k{1}"].push(f"q{i}"); i += 1; }
    let x = t.0[f"k{1}"].pop();
    match x { Some(s) => println(f"{s}"), None => println("none") }
    println(f"a:{t.0[f"k{1}"].len()} {t.1}");
}
fn sorted() {
    let mut sm: SortedMap[i64, Vec[String]] = SortedMap.new();
    sm.insert(1, vec![f"a{1}"]);
    sm.insert(2, vec![f"b{2}"]);
    let mut w: (i64, SortedMap[i64, Vec[String]]) = (5, sm);
    w.1[2].push(f"c{3}");
    w.1[1].clear();
    println(f"b:{w.1[1].len()} {w.1[2].len()} {w.0}");
}
fn main() {
    hashed();
    sorted();
}
"#,
        &["q2", "a:4 3", "b:0 2 5"],
        "asan_method_on_map_value_through_tuple_element",
    );
}
