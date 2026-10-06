//! B-2026-10-05-109 -- a `Vec.filled(n, v)` with no destination type (a `for`
//! iterable, an argument) bit-copied one heap value into every slot, so
//! `for s in Vec.filled(2, base.clone()) { .. }` freed the same buffer once
//! per slot. A `for` over a repeat literal (`[v; n]`, `Vec[v; n]`) also never
//! freed the temporary.

use super::*;

/// `Vec.filled` and repeat-literal temporaries iterated by value, read-only,
/// passed as an argument, nested inside a map walk, and over scalars.
#[test]
fn e2e_vec_filled_temp_loops_own_each_slot() {
    let src = r#"struct P { name: String, k: i64 }
fn total(v: Vec[String]) -> i64 { let mut n = 0; for s in v { n = n + s.len(); } return n; }
fn main() {
    let base = f"x{7}";
    let mut r: Vec[String] = Vec.new();
    for s in Vec.filled(2, base.clone()) { r.push(s); }
    for s in Vec.filled(2, base.clone()).into_iter() { r.push(s); }
    for s in Vec.filled(2, base.clone() + "y") { r.push(s); }
    for s in [base.clone(); 2] { r.push(s); }
    println(f"a:{r}");
    for s in Vec.filled(2, base.clone()) { println(f"b:{s}"); }
    println(f"c:{total(Vec.filled(3, f"zz{1}"))}");
    let mut rows = 0;
    for row in Vec.filled(2, vec![f"q{1}", f"q{2}"]) { rows = rows + row.len(); }
    println(f"d:{rows}");
    let mut ks = 0;
    for p in Vec.filled(2, P { name: f"p{3}", k: 4 }) { ks = ks + p.k + p.name.len(); }
    println(f"e:{ks}");
    let mut m: SortedMap[i64, String] = SortedMap.new(); m.insert(1, f"m{1}"); m.insert(2, f"m{2}");
    let mut r3: Vec[String] = Vec.new();
    for (k, v) in m.iter() { for s in Vec.filled(2, v.clone() + f"{k}").into_iter() { r3.push(s); } }
    println(f"f:{r3}");
    let mut n = 0;
    for x in Vec.filled(3, 5) { n = n + x; }
    println(f"g:{n}");
    let mut z = 0;
    for x in [4; 3] { z = z + x; }
    for x in Vec[0; 3] { z = z + x; }
    for row in [vec![1, 2]; 2] { z = z + row.len(); }
    for s in [f"w{1}"; 2] { z = z + s.len(); }
    println(f"h:{z}");
}
"#;
    let want = "a:[x7, x7, x7, x7, x7y, x7y, x7, x7]\nb:x7\nb:x7\nc:9\nd:4\ne:12\nf:[m11, m11, m22, m22]\ng:15\nh:20\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
