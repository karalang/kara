//! B-2026-10-05-95: a `for` loop over a fresh SortedSet / SortedMap
//! of heap keys frees the temporary once.

use super::*;

/// The loop materializes the unnamed SortedSet / SortedMap into a hidden
/// local and frees it, with every String key and value, when the frame ends.
#[test]
fn asan_for_over_fresh_sorted_set_and_map_of_strings() {
    assert_clean_asan_run(
        r#"fn ss(n: i64) -> SortedSet[String] {
    let mut s: SortedSet[String] = SortedSet.new();
    for i in 0..n { s.insert(f"key-longer-than-the-inline-limit-{n - i}"); }
    return s;
}
fn sm(n: i64) -> SortedMap[String, String] {
    let mut m: SortedMap[String, String] = SortedMap.new();
    for i in 0..n { m.insert(f"k{n - i}", f"value-longer-than-the-inline-limit-{i}"); }
    return m;
}
fn main() {
    let a = ss(4);
    let b = ss(3);
    for s in a.intersection(b) { println(s.len()); }
    let mut total = 0;
    for s in ss(5) { total += s.len(); }
    println(total);
    for (k, v) in sm(2) { println(f"{k} {v.len()}"); }
    println("end");
}
"#,
        &["34", "34", "34", "170", "k1 36", "k2 36", "end"],
        "for_over_fresh_sorted_set_and_map_of_strings",
    );
}
