//! B-2026-10-04-43 -- a field read straight off a CLOSURE call's struct result
//! (`h(mk(47)).id`) failed to build with `cannot resolve field`, while binding
//! the result first built and was right. `--interp` was correct.

use super::*;

/// A struct with a `Drop` body, nested field chains and a method on the field,
/// read off identity and constructing closures (one of them capturing).
#[test]
fn e2e_field_read_off_closure_call_struct_result() {
    let src = r#"struct P { x: i64, y: i64 }
struct R { id: i64, name: String, p: P }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Q { s: String, n: i64 }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", p: P { x: i * 2, y: 0 } }; }
fn main() {
    let base = 100;
    let h = |x: R| x;
    let g = |n: i64| Q { s: f"q{n + base}", n: n };
    println(f"r{h(mk(47)).id}");
    println(f"a:{h(mk(1)).name}");
    println(f"b:{h(mk(2)).p.x}");
    println(f"c:{g(5).s} {g(6).n}");
    let t = g(7).s;
    println(f"d:{t} {h(mk(3)).name.len()}");
    println("end");
}
"#;
    let want = "r47\ndR47\na:h1\ndR1\nb:4\ndR2\nc:q105 6\nd:q107 2\ndR3\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A GENERIC struct result (`Bx[String]`) and a `shared` one: the generic
/// temporary must be dropped through its instantiation, or its `String` leaks.
#[test]
fn e2e_field_read_off_closure_call_generic_and_shared_result() {
    let src = r#"struct Bx[T] { a: T, b: i64 }
shared struct S { v: i64, t: String }
fn main() {
    let h = |x: Bx[String]| x;
    println(f"a:{h(Bx { a: "s".to_string(), b: 1 }).a} {h(Bx { a: f"w{2}", b: 4 }).b}");
    let k = |n: i64| Bx { a: f"q{n}", b: n };
    println(f"b:{k(3).a} {k(5).b}");
    let m = |n: i64| S { v: n, t: f"t{n}" };
    println(f"c:{m(3).v} {m(4).t}");
    println("end");
}
"#;
    let want = "a:s 4\nb:q3 5\nc:3 t4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
