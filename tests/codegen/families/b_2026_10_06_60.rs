//! B-2026-10-06-60 -- a `for` over a tuple's `Map` element freed the map as a
//! temporary and the tuple freed it again. The loop borrows the element, so
//! the map is still readable after it.

use super::*;

#[test]
fn e2e_for_over_tuple_map_leaves_it_readable() {
    let src = r#"fn main() {
    let mut m: Map[i64, String] = Map.new(); m.insert(2, f"b{2}"); m.insert(1, f"a{1}");
    let t = (m, 3);
    let mut n = 0;
    for (k, v) in t.0 { n = n + k + v.len(); }
    for (k, v) in t.0 { n = n + k; }
    let mut s: SortedMap[i64, String] = SortedMap.new(); s.insert(2, f"y{2}"); s.insert(1, f"x{1}");
    let u: (SortedMap[i64, String], i64) = (s, 4);
    for (k, v) in u.0 { println(f"{k}={v}"); }
    println(f"{n} {t.1} {u.1}");
}
"#;
    let want = "1=x1\n2=y2\n10 3 4\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
