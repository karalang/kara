//! B-2026-10-06-77 -- a `let` bound to an `if` or `match` that picks one of
//! two `ref` params lost its type in codegen: `longer.len()` had no handler
//! and `for d in longer` was not an iterable. A direct rebind (`let l = a`)
//! always worked; the branch form now takes the same re-borrow path.

use super::*;

#[test]
fn e2e_let_bound_to_branch_choosing_a_ref_param() {
    let src = r#"fn total(a: ref Vec[i64], b: ref Vec[i64]) -> i64 {
    let longer = if a.len() > b.len() { a } else { b };
    let mut s = 0;
    for d in longer { s += d; }
    for d in longer.iter() { s += d; }
    longer.len() + s
}
fn three(a: ref Vec[i64], b: ref Vec[i64], c: ref Vec[i64], k: i64) -> i64 {
    let w: ref Vec[i64] = if k == 0 { a } else if k == 1 { b } else { c };
    let mut s = 0;
    for d in w { s += d; }
    s * 10 + w.len()
}
fn pick(a: ref String, b: ref String) -> i64 {
    let longer = if a.len() > b.len() { a } else { b };
    let m = match a.len() { 0 => b, _ => a };
    longer.len() * 100 + m.len()
}
fn joined(a: ref Vec[String], b: ref Vec[String]) -> String {
    let v = match b.len() > a.len() { true => b, false => a };
    let mut out = "";
    for s in v { out = out + s; }
    out + f"{v.len()}"
}
fn main() {
    let a = [1, 2, 3];
    let b = [4, 5];
    let c = [6];
    println(f"{total(a, b)} {total(b, a)}");
    println(f"{three(a, b, c, 0)} {three(a, b, c, 1)} {three(a, b, c, 2)}");
    let x = "hello";
    let y = "hi";
    println(f"{pick(x, y)} {pick(y, x)}");
    let p = ["p", "q"];
    let r = ["r", "s", "t"];
    println(joined(p, r));
    println(f"{a.len()} {b.len()} {x} {y} {p.len()} {r.len()}");
}
"#;
    let want = "15 15\n63 92 61\n505 502\nrst3\n3 2 hello hi 2 3\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
