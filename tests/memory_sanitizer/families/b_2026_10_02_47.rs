//! B-2026-10-02-47: an empty result built from an over-allocated scratch buffer is freed

use super::*;

/// B-2026-10-02-47 — `SortedMap.range`, `Column.sorted()` / `argsort()` and
/// `Stats.sort` / `argsort` malloc a scratch buffer, fill it, and hand it out
/// as the result Vec with `len == cap == count`. When nothing matched, `cap`
/// was 0, which reads as a static buffer, so nothing ever freed it: one leaked
/// block per empty result. Cells cover an interval that misses every key, an
/// inverted interval, an empty map, `String` keys and values, a result bound
/// to a `let`, iterated as a temporary, and returned from a function, the
/// non-empty cases beside them, and the all-null and empty `Column`s.
#[test]
fn asan_empty_scratch_backed_vec_results_are_freed() {
    assert_clean_asan_run(
        r#"fn window(lo: i64, hi: i64) -> i64 {
    let mut seen: SortedMap[i64, i64] = SortedMap.new();
    seen.insert(0, 1);
    seen.insert(5, 2);
    let mut total = 0;
    for (_, c) in seen.range(lo, hi) {
        total += c;
    }
    return total;
}
fn keys_in(lo: i64, hi: i64) -> Vec[(i64, i64)] {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    m.insert(3, 30);
    m.insert(7, 70);
    return m.range(lo, hi);
}
fn main() {
    println(f"w{window(0, 10)} w{window(1, 4)} w{window(20, 30)} w{window(9, 2)} w{window(5, 5)}");
    let mut e: SortedMap[i64, i64] = SortedMap.new();
    let r = e.range(0, 100);
    println(f"e{r.len()}");
    e.insert(1, 1);
    let r2 = e.range(2, 3);
    println(f"e{r2.len()}");
    let mut s: SortedMap[String, String] = SortedMap.new();
    s.insert("b", "bee");
    s.insert("d", "dee");
    let none = s.range("x", "z");
    let some = s.range("a", "c");
    println(f"s{none.len()} s{some.len()} {some[0].1}");
    println(f"k{keys_in(4, 6).len()} k{keys_in(0, 9).len()}");
    let mut n: Column[i64] = Column.with_capacity(3);
    n.push_null();
    n.push_null();
    let ns: Vec[i64] = n.sorted();
    let na: Vec[i64] = n.argsort();
    println(f"c{ns.len()} c{na.len()}");
    let ec: Column[i64] = Column.with_capacity(0);
    let es: Vec[i64] = ec.sorted();
    println(f"c{es.len()}");
    let mut f: Column[f64] = Column.with_capacity(2);
    f.push_null();
    let fs: Vec[f64] = f.sorted();
    println(f"c{fs.len()}");
    let xs: Vec[i64] = Vec.new();
    let st: Vec[i64] = Stats.sort(xs);
    let sa: Vec[i64] = Stats.argsort(xs);
    println(f"t{st.len()} t{sa.len()}");
    println("end");
}
"#,
        &[
            "w3 w0 w0 w0 w2",
            "e0",
            "e0",
            "s0 s1 bee",
            "k0 k2",
            "c0 c0",
            "c0",
            "c0",
            "t0 t0",
            "end",
        ],
        "asan_empty_scratch_backed_vec_results_are_freed",
    );
}
