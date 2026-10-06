//! B-2026-10-06-73: an empty `Vec` literal in a tuple slot

use super::*;

/// B-2026-10-06-73: `(vec![], 5)` against `(Vec[i64], i64)` was refused
/// at check time; it now takes the slot's type, and every compiled position
/// (a let, a nested literal in a `Vec`, a return value, a struct field, and a
/// tuple whose empty `Vec` is later grown with a heap `String`) prints what the
/// interpreter prints.
#[test]
fn e2e_empty_vec_literal_in_a_tuple_slot() {
    let src = r#"struct P {
    t: (Vec[i64], i64),
}

fn mk() -> (Vec[String], i64) {
    return (vec![], 3);
}

fn main() {
    let t: (Vec[i64], i64) = (vec![], 5);
    println(f"{t.0.len()} {t.1}");
    let v: Vec[(Vec[i64], i64)] = vec![(vec![1], 5), (vec![], 6)];
    let mut total = 0;
    for (xs, k) in v {
        total += xs.len() + k;
    }
    println(f"{total}");
    let p = P { t: (Vec[], 9) };
    println(f"{p.t.0.len()} {p.t.1}");
    let (a, b) = mk();
    println(f"{a.len()} {b}");
    let mut w: (Vec[String], i64) = (vec![], 1);
    w.0.push(f"grown-to-a-heap-string-{w.1}");
    println(f"{w.0[0]}");
}
"#;
    assert_eq!(
        run_program(src),
        Some("0 5\n12\n0 9\n0 3\ngrown-to-a-heap-string-1\n".to_string())
    );
}
