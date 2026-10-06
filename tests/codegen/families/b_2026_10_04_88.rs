//! B-2026-10-04-88 -- a tuple holding a `Map` freed the handle but not the
//! values' heap, and a tuple that also held a `Vec` leaked the whole map. The
//! output was always right; this pins it across the moves the fix touches.

use super::*;

#[test]
fn e2e_tuple_held_map_moves_and_reads() {
    let src = r#"struct P { s: String }
fn mk() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a{1}", 1);
    m
}
fn eat(t: (Map[String, i64], Vec[i64])) -> i64 { t.1.len() + t.0.len() }
fn main() {
    let mut m: Map[i64, P] = Map.new();
    m.insert(1, P { s: f"k{1}" });
    let tm = (m, vec![1]);
    let u = tm;
    let (a, b) = (mk(), vec![1, 2, 3]);
    println(f"{u.0.len()} {u.1.len()} {eat((mk(), vec![2]))} {a.len()} {b.len()}");
}
"#;
    let want = "1 1 2 1 3\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
