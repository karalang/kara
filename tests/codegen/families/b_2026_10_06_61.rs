//! B-2026-10-06-61 -- a method on a map value reached through a tuple element
//! (`t.0[k].push(x)`) failed `karac build` with "indexed-receiver method
//! requires the indexed container to be a named variable".

use super::*;

#[test]
fn e2e_method_on_map_value_through_tuple_element() {
    let src = r#"fn main() {
    let mut m: Map[String, Vec[String]] = Map.new();
    m.insert(f"k{1}", vec![f"x{1}"]);
    let mut t = (m, 3);
    t.0[f"k{1}"].push(f"y{2}");
    let mut m2: Map[String, Vec[String]] = Map.new();
    m2.insert("a".to_string(), vec!["p".to_string()]);
    let mut u: (Map[String, Vec[String]], Vec[i64]) = (m2, vec![1]);
    let mut i = 0;
    while i < 3 { u.0["a"].push(f"q{i}"); i += 1; }
    let x = u.0["a"].pop();
    match x { Some(s) => println(f"{s}"), None => println("none") }
    let mut sm: SortedMap[i64, Vec[i64]] = SortedMap.new();
    sm.insert(1, vec![10]);
    sm.insert(2, vec![20, 21]);
    let mut w = (5, sm);
    w.1[2].push(22);
    w.1[1].push(11);
    w.1[1].clear();
    println(f"{t.0[f"k{1}"].len()} {t.1} {u.0["a"].len()} {u.1.len()}");
    println(f"{w.1[1].len()} {w.1[2].len()} {w.1[2][2]} {w.0}");
}
"#;
    let want = "q2\n2 3 3 1\n0 3 22 5\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
