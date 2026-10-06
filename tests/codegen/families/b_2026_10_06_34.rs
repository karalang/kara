//! B-2026-10-06-34 -- a closure param typed by the enclosing generic fn's `T`
//! (`|x: T| x` inside `w[T = String]`) was registered as bare `T`, so handing
//! it back skipped the copy a caller-retained param needs and the argument was
//! freed twice. B-2026-10-06-33 -- a generic struct literal over that `T`
//! (`|x: T| Bx { a: x, b: 3 }`) was laid out with `T` unresolved and failed
//! module verification.

use super::*;

/// `|x: T| x` inside a monomorph over a heap-backed `T`: a temp, a named
/// local, a `Vec`, a tail return of the closure call, and the `i64` control.
#[test]
fn e2e_generic_closure_hands_back_t_param() {
    let src = r#"fn w[T](v: T) -> i64 { let k = |x: T| x; let r = k(v); return 3; }
fn back[T](v: T) -> T { let k = |x: T| x; return k(v); }
fn main() {
    let s = "named".to_string();
    println(f"a:{w("s".to_string())} {w(s)} {w(vec![1, 2])} {w(5)}");
    println(f"b:{back("t".to_string())} {back(vec!["u".to_string()]).len()} {back(7)}");
    println("end");
}
"#;
    let want = "a:3 3 3 3\nb:t 1 7\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// A generic struct built over the enclosing fn's `T` (and over two params,
/// and over `Vec[T]`), read inline and through a binding.
#[test]
fn e2e_generic_closure_builds_struct_over_t() {
    let src = r#"struct Bx[T] { a: T, b: i64 }
struct Pr[A, B] { x: A, y: B }
fn wrapit[T](v: T) -> i64 { let k = |x: T| Bx { a: x, b: 3 }; return k(v).b; }
fn keep[T](v: T) -> T { let k = |x: T| Bx { a: x, b: 4 }; let r = k(v); return r.a; }
fn pair[A, B](a: A, b: B) -> B { let k = |p: A, q: B| Pr { x: p, y: q }; let r = k(a, b); return r.y; }
fn lens[T](v: Vec[T]) -> i64 { let k = |x: Vec[T]| Bx { a: x, b: 1 }; let r = k(v); return r.a.len() + r.b; }
fn main() {
    println(f"a:{wrapit("s".to_string())} {wrapit(7)} {wrapit(2.5)}");
    println(f"b:{keep("kept".to_string())} {keep(9)}");
    println(f"c:{pair(1, "two".to_string())} {pair("one".to_string(), 2)}");
    println(f"d:{lens(vec!["a".to_string(), "b".to_string()])} {lens(vec![1, 2, 3])}");
    println("end");
}
"#;
    let want = "a:3 3 3\nb:kept 9\nc:two 2\nd:3 4\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
