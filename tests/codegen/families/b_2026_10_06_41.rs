//! B-2026-10-06-41: a `const` declared `Array[T, N]`

use super::*;

/// B-2026-10-06-41: `const NUMS: Array[i64, 3] = [10, 20, 30];` was
/// refused by the typechecker on every surface. Bound to a local, the const
/// is a by-value array that the compiled backends index, measure and walk.
#[test]
fn e2e_const_array_annotation_builds_and_runs() {
    let Some(out) = run_program(
        r#"const NUMS: Array[i64, 3] = [10, 20, 30];
const LINES: Array[(i64, i64, i64), 2] = [(1, 3, 2), (4, 6, 5)];
fn main() {
    let a = NUMS;
    let mut s = 0;
    for x in a {
        s += x;
    }
    println(f"{a[1]} {a.len()} {s}");
    let b = LINES;
    for (x, y, mid) in b {
        println(f"{x}-{y} over {mid}");
    }
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "20 3 60\n1-3 over 2\n4-6 over 5\n");
}
