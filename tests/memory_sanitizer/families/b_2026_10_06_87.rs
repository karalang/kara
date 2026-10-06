//! B-2026-10-06-87 -- a `PriorityQueue` of tuples failed `karac build`; with
//! a heap element in the tuple, comparing and moving elements must neither
//! leak nor free twice.

use super::*;

#[test]
fn asan_priority_queue_of_heap_tuples() {
    assert_clean_asan_run(
        r#"fn main() {
    let mut pq: PriorityQueue[(i64, String)] = PriorityQueue.new();
    pq.push((3, f"c{3}"));
    pq.push((1, f"a{1}"));
    pq.push((2, f"b{2}"));
    pq.push((1, f"a{0}"));
    let first = pq.pop();
    match first { Some((k, s)) => println(f"{k} {s}"), None => println("none") }
    let mut n = 0;
    while let Some((k, s)) = pq.pop() { n = n + k + s.len(); }
    let v = vec![(f"x{1}", 2), (f"x{1}", 1)];
    println(f"{n} {v[0].cmp(v[1]) == Ordering.Greater}");
}
"#,
        &["1 a0", "12 true"],
        "asan_priority_queue_of_heap_tuples",
    );
}
