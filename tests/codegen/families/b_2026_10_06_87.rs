//! B-2026-10-06-87 -- `.cmp()` on an indexed tuple element had no codegen
//! dispatch, so `v[0].cmp(v[1])` over `Vec[(i64, i64)]` failed `karac build`
//! and so did every `PriorityQueue` of tuples (its `outranks` compares
//! `self.xs[i]` against `self.xs[j]`).

use super::*;

#[test]
fn e2e_tuple_priority_queue_and_indexed_tuple_cmp() {
    let src = r#"fn main() {
    let mut pq: PriorityQueue[(i64, i64)] = PriorityQueue.new();
    pq.push((3, 1));
    pq.push((1, 2));
    pq.push((1, 0));
    while let Some((a, b)) = pq.pop() { println(f"{a} {b}"); }
    let mut q3: PriorityQueue[(i64, i64, i64)] = PriorityQueue.new();
    let mut i = 0;
    while i < 5 { q3.push((10 - i, i, i * i)); i += 1; }
    let mut out = 0;
    while let Some((_a, b, _c)) = q3.pop() { out = out * 10 + b; }
    let v = vec![(1, 2), (0, 5)];
    println(f"{out} {v[0].cmp(v[1]) == Ordering.Greater} {v[1].cmp(v[0]) == Ordering.Less} {v[0].cmp(v[0]) == Ordering.Equal}");
}
"#;
    let want = "1 0\n1 2\n3 1\n43210 true true true\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
