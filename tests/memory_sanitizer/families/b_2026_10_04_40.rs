//! B-2026-10-04-40 — a tuple holding a nested fixed array of heap elements
//! (`(Array[Array[String, 2], 2], i64)`) frees every inner element.

use super::*;

/// The tuple drop's admission gate asked whether an `Array` element's element
/// owned heap, and `type_expr_has_drop_heap` answers `false` for an `Array`
/// path, so the nested spelling got no drop at all and leaked every inner
/// `String`. The fresh-temp argument registrar and the by-value param gates
/// asked the same one-level question, and they have to move with the drop
/// gate or a named tuple handed through `thru` frees twice. Pins the local,
/// a move, a returned tuple pushed into a `Vec`, a three-deep array, and a
/// named and a fresh argument both eaten and handed back.
#[test]
fn asan_tuple_nested_array_of_strings_frees_inner_elements() {
    assert_clean_asan_run(
        r#"fn mk() -> (Array[Array[String, 2], 2], i64) { ([[f"a{1}", f"b{2}"], [f"c{3}", f"d{4}"]], 3) }
fn eat(t: (Array[Array[String, 2], 2], i64)) -> i64 { t.1 }
fn thru(t: (Array[Array[String, 2], 2], i64)) -> (Array[Array[String, 2], 2], i64) { t }
fn main() {
    let nested: (Array[Array[String, 2], 2], i64) = ([[f"a{1}", f"b{2}"], [f"c{3}", f"d{4}"]], 3);
    println(f"{nested.1}");
    let m = mk();
    let moved = m;
    println(f"{moved.1} {moved.0[0][1]}");
    let mut v: Vec[(Array[Array[String, 2], 2], i64)] = Vec.new();
    v.push(mk());
    v.push(mk());
    println(f"{v.len()}");
    let deep: (Array[Array[Array[String, 1], 2], 1], i64) = ([[[f"x{1}"], [f"y{2}"]]], 4);
    println(f"{deep.1}");
    let named = mk();
    println(f"{eat(named)}");
    println(f"{eat(([[f"p{1}", f"q{2}"], [f"r{3}", f"s{4}"]], 9))}");
    let h = mk();
    let u = thru(h);
    println(f"{u.1} {u.0[1][0]}");
    let w = thru(mk());
    println(f"{w.1} {w.0[1][1]}");
}
"#,
        &["3", "3 b2", "2", "4", "3", "9", "3 c3", "3 d4"],
        "tuple_nested_array_of_strings",
    );
}
