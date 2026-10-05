//! B-2026-10-05-97 -- a struct literal whose fields are written out of
//! declaration order stored each value at its WRITTEN position when compiled:
//! `F { b: 3, a: 5 }` printed `a=3 b=5`, and differently typed fields failed
//! module verification. `--interp` was correct.

use super::*;

/// Nested, mixed-type and `shared` literals written out of order; the field
/// initializers still run in the order written.
#[test]
fn e2e_struct_literal_fields_out_of_declaration_order() {
    let src = r#"struct P { x: i64, y: i64 }
struct Q { a: P, b: i64 }
struct G { a: i64, s: String }
shared struct H { a: i64, b: i64, t: String }
struct R { v: Vec[i64], tag: String, n: i64 }
fn id(x: i64) -> i64 { return x; }
fn noisy(tag: String, v: i64) -> i64 { println(f"eval {tag}"); return v; }
fn main() {
    let n = id(3);
    let q = Q { b: n, a: P { y: 0, x: 5 } };
    println(f"q:{q.b} {q.a.x} {q.a.y}");
    let g = G { s: "hi".to_string(), a: id(7) };
    println(f"g:{g.a} {g.s}");
    let h = H { t: "ht".to_string(), b: 3, a: 5 };
    println(f"h:{h.a} {h.b} {h.t}");
    let r = R { n: noisy("n", 4), tag: "rt".to_string(), v: vec![noisy("v", 1), 2] };
    println(f"r:{r.n} {r.tag} {r.v.len()} {r.v[0]}");
}
"#;
    let want = "q:3 5 0\ng:7 hi\nh:5 3 ht\neval n\neval v\nr:4 rt 2 1\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A generic literal (directly and inside a monomorph), a `Map.new()` field,
/// and a spread over an out-of-order base.
#[test]
fn e2e_struct_literal_out_of_order_generic_map_and_spread() {
    let src = r#"struct Bx[T] { a: T, b: i64 }
struct C { m: Map[i64, String], k: i64 }
struct F { a: i64, b: i64, c: String }
fn mk[T](v: T) -> Bx[T] { return Bx { b: 2, a: v }; }
fn main() {
    let x = Bx { b: 1, a: "s".to_string() };
    println(f"x:{x.a} {x.b}");
    let y = mk(4.5);
    println(f"y:{y.a} {y.b}");
    let mut c = C { k: 9, m: Map.new() };
    c.m.insert(1, "one".to_string());
    println(f"c:{c.k} {c.m.len()}");
    let f = F { c: "fc".to_string(), b: 2, a: 1 };
    let f2 = F { b: 20, ..f };
    println(f"f:{f2.a} {f2.b} {f2.c}");
}
"#;
    let want = "x:s 1\ny:4.5 2\nc:9 1\nf:1 20 fc\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
