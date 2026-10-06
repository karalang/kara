//! B-2026-10-06-40: `len` / `is_empty` on a fixed array of any element type

use super::*;

/// B-2026-10-06-40: `Array[(i64, i64), 2].len()` was refused by the
/// typechecker, as was `is_empty`, for every non-scalar element type. Both
/// are the array's own `N`, owned or behind `ref`, including an empty array.
#[test]
fn e2e_array_len_and_is_empty_for_every_element_type() {
    let Some(out) = run_program(
        r#"fn count(xs: ref Array[(i64, i64), 2]) -> i64 {
    if xs.is_empty() {
        return 0;
    }
    xs.len()
}

fn main() {
    let b: Array[(i64, i64), 2] = [(1, 2), (3, 4)];
    let s: Array[String, 3] = ["a", "bb", "ccc"];
    let v: Array[Vec[i64], 1] = [[1, 2]];
    let o: Array[Option[i64], 2] = [Some(1), None];
    let e: Array[String, 0] = Array[];
    println(f"{b.len()} {b.is_empty()} {s.len()} {s.is_empty()} {v.len()} {o.len()} {e.len()} {e.is_empty()} {count(b)}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "2 false 3 false 1 2 0 true 2\n");
}
