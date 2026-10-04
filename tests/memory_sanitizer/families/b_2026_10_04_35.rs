//! B-2026-10-04-35 -- nested index reads and stores through a container held
//! in a tuple element: displaced `String`s are released once.

use super::*;

/// `t.0[i][j]` reads and stores over `Array` and `Vec[Vec[String]]` tuple
/// elements, including one inside a struct field: no leak, no double free.
#[test]
fn asan_nested_index_through_tuple_element() {
    assert_clean_asan_run(
        r#"struct H { t: (Vec[Vec[String]], i64) }
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
"#,
        &["a:30 2 5", "b:w4 z3 x1", "c:q2", "d:37"],
        "nested_index_through_tuple_element",
    );
}
