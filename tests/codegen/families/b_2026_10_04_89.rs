//! B-2026-10-04-89 -- a tuple-element assignment through a `Map` value
//! (`m[1].1 = 7`) failed the build with "tuple-element assignment through
//! this receiver shape is not yet lowered" while `--interp` applied it.

use super::*;

/// Each store lands in the map: a scalar element and `+=`, a `String`
/// element under a `String` key, a whole struct element, through a
/// `mut ref` map param, and a map held in a `Vec` (scalar, `String`, and a
/// field store through the same nested map value).
#[test]
fn e2e_tuple_element_store_through_map_value() {
    let src = r#"struct P { n: i64, s: String }
fn bump(m: mut ref Map[String, (P, String)]) { m["a"].1 = f"t{5}"; }
fn main() {
    let mut m: Map[i64, (P, i64)] = Map.new();
    m.insert(1, (P { n: 5, s: f"a{1}" }, 2));
    m[1].1 = 7;
    m[1].1 += 3;
    println(f"a:{m[1].1} {m[1].0.n}");
    let mut ms: Map[String, (P, String)] = Map.new();
    ms.insert(f"a", (P { n: 1, s: f"b{1}" }, f"c{1}"));
    ms["a"].1 = f"d{2}";
    println(f"b:{ms["a"].1} {ms["a"].0.s}");
    ms["a"].0 = P { n: 9, s: f"e{3}" };
    println(f"c:{ms["a"].0.n} {ms["a"].0.s}");
    bump(mut ms);
    println(f"d:{ms["a"].1}");
    let mut vm: Vec[Map[i64, (i64, String)]] = Vec.new();
    let mut m2: Map[i64, (i64, String)] = Map.new();
    m2.insert(4, (1, f"f{4}"));
    vm.push(m2);
    vm[0][4].1 = f"g{4}";
    vm[0][4].0 = 44;
    println(f"e:{vm[0][4].0} {vm[0][4].1}");
    let mut vp: Vec[Map[i64, (P, i64)]] = Vec.new();
    let mut m3: Map[i64, (P, i64)] = Map.new();
    m3.insert(2, (P { n: 3, s: f"h{2}" }, 6));
    vp.push(m3);
    vp[0][2].0.n = 33;
    vp[0][2].0.s = f"hh{2}";
    vp[0][2].1 = 66;
    println(f"f:{vp[0][2].0.n} {vp[0][2].0.s} {vp[0][2].1}");
}
"#;
    let want = "a:10 5\nb:d2 b1\nc:9 e3\nd:t5\ne:44 g4\nf:33 hh2 66\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
