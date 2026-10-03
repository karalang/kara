//! B-2026-10-03-24: an unsigned 64-bit integer inside a tuple or a nested `Vec` orders as unsigned in the interpreter

use super::*;

/// B-2026-10-03-24: the i64 carrier makes `u64::MAX` and `-1` the same value,
/// and the unsigned dispatch only reached a TOP-LEVEL element, so a `u64`
/// inside a tuple or a nested `Vec` ordered as negative under `--interp`:
/// `(u64::MAX, 1) < (0, 1)` was `true`, `Vec[(u64, i8)].sort()` put the
/// values past `i64::MAX` first, `binary_search` then missed, and
/// `sort_by(|x, y| x.cmp(y))` disagreed with `sort()`. The expected text is
/// the compiled program's output; a signed tuple keeps its signed order.
#[test]
fn interp_unsigned_leaf_inside_tuple_orders_unsigned() {
    let out = run(r#"fn main() {
    let a: u64 = 18446744073709551615u64;
    let b: u64 = 0;
    println(f"{a < b} {(a, 1) < (b, 1)} {(1, a) > (1, b)} {(b, 1) <= (a, 0)}");
    let mut u: Vec[(u64, i8)] = Vec.new();
    u.push((18446744073709551615u64, -1));
    u.push((0, 5));
    u.push((9223372036854775808u64, -128));
    u.push((1, 127));
    println(f"{u.is_sorted()}");
    let s = u.sorted();
    println(f"{s}");
    u.sort();
    println(f"{u} {u.is_sorted()}");
    println(f"{u.binary_search((9223372036854775808u64, -128))}");
    let mut nv: Vec[Vec[u64]] = Vec.new();
    nv.push(vec![18446744073709551615u64]);
    nv.push(vec![3]);
    nv.sort();
    println(f"{nv}");
    let mut c: Vec[(u64, i64)] = Vec.new();
    c.push((18446744073709551615u64, 1));
    c.push((2, 1));
    c.sort_by(|x, y| x.cmp(y));
    println(f"cmp {c}");
    let arr: Array[u64, 2] = [18446744073709551615u64, 0];
    let arr2: Array[u64, 2] = [0, 0];
    println(f"arr {arr > arr2}");
    let neg = (-1, 2) < (0, 2);
    println(f"signed {neg}");
}
"#);
    assert_eq!(out, "false false true true\nfalse\n[(0, 5), (1, 127), (9223372036854775808, -128), (18446744073709551615, -1)]\n[(0, 5), (1, 127), (9223372036854775808, -128), (18446744073709551615, -1)] true\nSome(2)\n[[3], [18446744073709551615]]\ncmp [(2, 1), (18446744073709551615, 1)]\narr true\nsigned true\n");
}
