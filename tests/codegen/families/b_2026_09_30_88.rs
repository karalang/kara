//! B-2026-09-30-88 -- an index store into a container held in a tuple
//! element (`a.0[0] = v` over `(Array[_, N], i64)`) failed the build with
//! "Index assignment target must be a variable" while `--interp` ran it.

use super::*;

/// The row's two programs folded together with their neighbours: `String`,
/// heap-field struct and plain struct elements, a computed index, a compound
/// assign, a tuple inside a struct field, a tuple inside a tuple, and a
/// `Vec` element replaced whole. Output matches the interpreter.
#[test]
fn e2e_index_store_into_array_held_in_tuple_element() {
    let src = r#"struct P { n: i64 }
struct Q { s: String, k: i64 }
struct H { t: (Array[i64, 2], i64) }
fn idx(i: i64) -> i64 { return i }
fn main() {
    let mut a: (Array[String, 2], i64) = ([f"x{1}", f"y{2}"], 7);
    a.0[1] = f"z{3}";
    println(f"s:{a.0[0]} {a.0[1]} {a.1}");
    let mut b: (i64, Array[Q, 2]) = (4, [Q { s: f"q{1}", k: 1 }, Q { s: f"q{2}", k: 2 }]);
    b.1[0] = Q { s: f"w{9}", k: 9 };
    println(f"q:{b.1[0].s} {b.1[0].k} {b.1[1].s} {b.0}");
    let mut c: (Array[i64, 3], Array[P, 2]) = ([1, 2, 3], [P { n: 1 }, P { n: 2 }]);
    let j = idx(2);
    c.0[j] = 30;
    c.1[idx(1)] = P { n: 20 };
    c.0[0] += 5;
    println(f"c:{c.0[0]} {c.0[1]} {c.0[2]} {c.1[0].n} {c.1[1].n}");
    let mut h = H { t: ([1, 2], 3) };
    h.t.0[1] = 8;
    println(f"h:{h.t.0[0]} {h.t.0[1]} {h.t.1}");
    let di: (Array[i64, 2], i64) = ([1, 2], 3);
    let mut d: ((Array[i64, 2], i64), i64) = (di, 4);
    d.0.0[0] = 11;
    println(f"d:{d.0.0[0]} {d.0.0[1]}");
    let mut f: (Array[Vec[i64], 2], i64) = ([[1], [2, 3]], 0);
    f.0[0] = [9, 9, 9];
    let ff: Array[Vec[i64], 2] = f.0;
    println(f"f:{ff[0].len()} {ff[1].len()} {ff[0][2]}");
    println("end");
}
"#;
    let want = "s:x1 z3 7\nq:w9 9 q2 4\nc:6 2 30 1 20\nh:1 8 3\nd:11 2\nf:3 2 9\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
