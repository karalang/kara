//! B-2026-10-04-35 -- a nested index through a container held in a tuple
//! element (`t.0[i][j]`) failed the build with "nested indexed read requires
//! the outer container to be a named variable", for an `Array` and a `Vec`.

use super::*;

/// Nested reads and stores through a tuple-held `Array[Array[..]]` and
/// `Vec[Vec[String]]`, through a struct field's tuple, and in a loop. Output
/// matches the interpreter.
#[test]
fn e2e_nested_index_through_tuple_element() {
    let src = r#"struct H { t: (Vec[Vec[String]], i64) }
fn main() {
    let mut e: (Array[Array[i64, 2], 2], i64) = ([[1, 2], [3, 4]], 5);
    e.0[1][0] = 30;
    println(f"a:{e.0[1][0]} {e.0[0][1]} {e.1}");
    let mut v: (i64, Vec[Vec[String]]) = (0, [[f"x{1}"], [f"y{2}", f"z{3}"]]);
    v.1[1][0] = f"w{4}";
    println(f"b:{v.1[1][0]} {v.1[1][1]} {v.1[0][0]}");
    let mut h = H { t: ([[f"p{1}"]], 0) };
    h.t.0[0][0] = f"q{2}";
    println(f"c:{h.t.0[0][0]}");
    let mut s = 0;
    let mut i = 0;
    while i < 2 { let mut j = 0; while j < 2 { s += e.0[i][j]; j += 1; } i += 1; }
    println(f"d:{s}");
}
"#;
    let want = "a:30 2 5\nb:w4 z3 x1\nc:q2\nd:37\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
