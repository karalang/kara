//! B-2026-10-04-38: a tuple annotation reaches the elements of a nested tuple literal

use super::*;

/// B-2026-10-04-38: a tuple annotation's element types reached only the
/// OUTER tuple literal's elements, so a nested literal synthesised on its
/// own and `(([1, 2], 3), 4)` against `((Array[i64, 2], i64), i64)` was
/// rejected as `found '((Vec[i64], i64), i64)'`. The expectation now
/// recurses: an array literal, a repeat literal and an inferred `Vec.new()`
/// each meet their slot at depth two and three.
#[test]
fn e2e_tuple_annotation_reaches_a_nested_tuple_literal() {
    let out = run_program(
        r#"fn main() {
    let mut d: ((Array[i64, 2], i64), i64) = (([1, 2], 3), 4);
    d.0.0[0] = 11;
    println(f"d:{d.0.0[0]} {d.0.0[1]} {d.0.1} {d.1}");
    let t: (((Array[i64, 3], i64), i64), i64) = ((([0; 3], 5), 6), 7);
    println(f"t:{t.0.0.0[2]} {t.0.0.1} {t.0.1} {t.1}");
    let mut e: ((Vec[i64], i64), i64) = ((Vec.new(), 1), 2);
    e.0.0.push(9);
    println(f"e:{e.0.0.len()} {e.0.0[0]} {e.0.1} {e.1}");
}"#,
    );
    assert_eq!(out.as_deref(), Some("d:11 2 3 4\nt:0 5 6 7\ne:1 9 1 2\n"));
}
