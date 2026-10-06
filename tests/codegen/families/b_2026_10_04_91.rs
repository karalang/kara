//! B-2026-10-04-91 -- a tuple holding a `Map` / `SortedMap` element
//! (`let t = (m, 3)` with an annotated `m: Map[i64, String]`) lost the map's
//! key and value types, so `t.0[k]`, `t.0[k].f = v`, destructuring and passing
//! the tuple by value failed to build or leaked the map's values.

use super::*;

/// Index, field write through index, `get`, destructure, by-value pass, and
/// an unused map element, over `Map` and `SortedMap` tuple elements.
#[test]
fn e2e_map_in_tuple_keeps_key_and_value_types() {
    let src = r#"struct P { n: i64 }
fn take(t: (Map[i64, String], i64)) -> i64 { return t.0.len() + t.1; }
fn g(t: mut ref (Map[i64, P], i64)) { t.0[1].n = 9; }
fn r1() { let mut m: Map[i64, P] = Map.new(); m.insert(1, P { n: 5 }); let mut t = (m, 1); g(mut t); println(f"r1:{t.0[1].n}"); }
fn w1() { let mut m: Map[i64, P] = Map.new(); m.insert(1, P { n: 5 }); m.insert(2, P { n: 6 }); let mut t = (m, 1); t.0[2].n = 60; println(f"w1:{t.0[1].n} {t.0[2].n} {t.0.len()}"); }
fn w2() { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"a{1}"); let t = (m, 3); println(f"w2:{t.0[1]} {t.0.len()}"); }
fn w4() { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"a{1}"); let t = (m, 3); let (mm, k) = t; println(f"w4:{mm[1]} {k}"); }
fn w5() { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"a{1}"); let t = (m, 3); println(f"w5:{take(t)}"); }
fn w8() { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"a{1}"); let t = (m, 3); match t.0.get(1) { Some(s) => println(f"w8:{s}"), None => println("none") } }
fn u2() { let mut m: Map[i64, String] = Map.new(); m.insert(1, f"s{1}"); let t = (m, 1); println(f"u2:{t.1}"); }
fn u3() { let mut m: Map[i64, P] = Map.new(); m.insert(1, P { n: 5 }); let t = (m, 1); println(f"u3:{t.1}"); }
fn s1() { let mut m: SortedMap[i64, String] = SortedMap.new(); m.insert(2, f"b{2}"); m.insert(1, f"a{1}"); let t = (m, 3); println(f"s1:{t.0[1]} {t.0.len()}"); }
fn main() { r1(); w1(); w2(); w4(); w5(); w8(); u2(); u3(); s1(); println("end"); }
"#;
    let want = "r1:9\nw1:5 60 2\nw2:a1 1\nw4:a1 3\nw5:4\nw8:a1\nu2:1\nu3:1\ns1:a1 2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
