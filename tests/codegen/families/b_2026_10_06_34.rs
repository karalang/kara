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
