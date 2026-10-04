//! B-2026-10-04-37 -- an `Array` moved out of a tuple element by an
//! unannotated `let` is owned once.

use super::*;

/// Heap-element arrays moved out of tuples by `let x = t.0`: no leak, no
/// double free, each `Drop` body once.
#[test]
fn asan_unannotated_let_of_tuple_element_array() {
    assert_clean_asan_run(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct P { n: i64 }
struct H { t: (Array[String, 2], i64) }
fn main() {
    let f: (Array[Vec[i64], 2], i64) = ([[1], [2, 3]], 7);
    let ff = f.0;
    println(f"a:{ff[1].len()} {ff[0][0]} {f.1}");
    let g: (i64, Array[String, 2]) = (0, [f"x{1}", f"y{2}"]);
    let gg = g.1;
    println(f"b:{gg[1]} {gg[0].len()}");
    let p: (Array[P, 2], i64) = ([P { n: 1 }, P { n: 2 }], 0);
    let pp = p.0;
    println(f"c:{pp[1].n}");
    let h = H { t: ([f"h{1}", f"i{2}"], 0) };
    let hh = h.t.0;
    println(f"d:{hh[0]}");
    {
        let r: (Array[R, 2], i64) = ([R { id: 1 }, R { id: 2 }], 0);
        let rr = r.0;
        println(f"e:{rr[1].id}");
    }
    println("end");
}
"#,
        &[
            "a:2 1 7", "b:y2 2", "c:2", "d:h1", "e:2", "dR1", "dR2", "end",
        ],
        "unannotated_let_tuple_element_array",
    );
}
