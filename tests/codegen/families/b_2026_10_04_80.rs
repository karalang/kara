//! B-2026-10-04-80 -- an index store through a tuple field of an element
//! whose subscript is a call (`hs[idx(0)].t.1[0] = 4`) failed `karac build`
//! with "Index assignment target must be a variable"; the identifier-subscript
//! spelling built and `--interp` ran both.

use super::*;

/// Call subscripts at both levels, an arithmetic subscript, a `String`
/// element replaced, and a `Vec` of tuples; each subscript is evaluated once,
/// in source order.
#[test]
fn e2e_index_store_through_tuple_under_call_subscript() {
    let src = r#"struct H { xs: Vec[i64], t: (i64, Vec[i64]), s: (String, Vec[String]) }
fn idx(k: i64) -> i64 { println(f"idx{k}"); return k }
fn main() {
    let mut hs: Vec[H] = [H { xs: [1], t: (0, [5, 9]), s: ("a".to_string(), ["p".to_string(), "q".to_string()]) }, H { xs: [2], t: (1, [6, 7]), s: ("b".to_string(), ["r".to_string()]) }];
    hs[idx(1)].t.1[idx(0)] = 40;
    hs[idx(0) + 1].t.1[1] = 41;
    hs[idx(0)].s.1[idx(1)] = "heap-replacement-string-x".to_string();
    println(f"{hs[1].t.1[0]} {hs[1].t.1[1]} {hs[0].s.1[1]} {hs[0].t.1[0]}");
    let mut ts: Vec[(i64, Vec[i64])] = [(0, [1, 2])];
    ts[idx(0)].1[idx(1)] = 8;
    println(f"{ts[0].1[1]}");
}
"#;
    let want = "idx1\nidx0\nidx0\nidx0\nidx1\n40 41 heap-replacement-string-x 5\nidx0\nidx1\n8\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
