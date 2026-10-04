//! B-2026-09-30-88 -- index stores into a container held in a tuple element.
//! The displaced element's heap is released exactly once, for an `Array`
//! element (newly lowered) and a `Vec` element (which leaked it before).

use super::*;

/// `String` and heap-field struct elements displaced through `t.0[i] = v`,
/// over an `Array` and a `Vec`, in a loop, and through a struct field's
/// tuple: no leak, no double free (the leak half is carried by the `-O0` leg).
#[test]
fn asan_index_store_into_tuple_element_container() {
    assert_clean_asan_run(
        r#"struct Q { s: String, k: i64 }
struct H { t: (Array[String, 2], i64) }
fn main() {
    let mut a: (Array[String, 2], i64) = ([f"x{1}", f"y{2}"], 7);
    a.0[1] = f"z{3}";
    println(f"s:{a.0[0]} {a.0[1]}");
    let mut b: (i64, Array[Q, 2]) = (4, [Q { s: f"q{1}", k: 1 }, Q { s: f"q{2}", k: 2 }]);
    b.1[0] = Q { s: f"w{9}", k: 9 };
    println(f"q:{b.1[0].s} {b.1[1].s}");
    let mut e: (Array[Q, 2], i64) = ([Q { s: f"e{1}", k: 1 }, Q { s: f"f{2}", k: 2 }], 0);
    let mut i = 0;
    while i < 2 { e.0[i] = Q { s: f"g{i}", k: i }; i += 1; }
    println(f"e:{e.0[0].s} {e.0[1].s}");
    let mut h = H { t: ([f"h{1}", f"i{2}"], 0) };
    h.t.0[1] = f"j{3}";
    println(f"h:{h.t.0[0]} {h.t.0[1]}");
    let mut d: (Vec[Q], i64) = ([Q { s: f"m{1}", k: 1 }], 0);
    d.0[0] = Q { s: f"n{2}", k: 2 };
    println(f"d:{d.0[0].s}");
    let mut c: (Vec[String], i64) = ([f"p{1}", f"q{2}"], 0);
    c.0[1] = f"r{3}";
    println(f"c:{c.0[0]} {c.0[1]}");
}
"#,
        &[
            "s:x1 z3", "q:w9 q2", "e:g0 g1", "h:h1 j3", "d:n2", "c:p1 r3",
        ],
        "index_store_tuple_element_container",
    );
}
