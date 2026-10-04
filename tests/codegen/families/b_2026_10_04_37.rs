//! B-2026-10-04-37 -- an unannotated `let x = t.0` of a tuple element typed
//! `Array[T, N]` recorded no element type, so `x[i].len()` / `x[i].n` failed
//! the build while the annotated spelling built.

use super::*;

/// `Vec`, `String`, plain-struct and `Drop`-struct array elements moved out
/// of a tuple (and out of a struct field's tuple) by an unannotated `let`.
/// Output, including each `Drop` body once, matches the interpreter.
#[test]
fn e2e_unannotated_let_of_tuple_element_array() {
    let src = r#"struct R { id: i64 }
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
"#;
    let want = "a:2 1 7\nb:y2 2\nc:2\nd:h1\ne:2\ndR1\ndR2\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
