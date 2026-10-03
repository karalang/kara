//! B-2026-10-03-23 -- a tuple literal pushed into a `Vec` of narrower
//! integer tuples wrote its default-width `i64` fields past the element slot.

use super::*;

/// B-2026-10-03-23 — `v.push((3, 4))` on a full `Vec[(i32, i32)]` stored a
/// 16-byte `{i64, i64}` into an 8-byte slot, writing 8 bytes past the
/// buffer (`Invalid write of size 8 ... 0 bytes after a block of size 16`
/// under valgrind), and the `Map` and `vec!` positions read the wrong fields.
#[test]
fn asan_narrow_tuple_literal_push_stays_in_its_slot() {
    assert_clean_asan_run(
        r#"fn main() {
    let mut v: Vec[(i32, i32)] = Vec.with_capacity(2);
    v.push((1, 2));
    v.push((3, 4));
    println(f"{v[0].0} {v[0].1} {v[1].0} {v[1].1}");
    let mut m: Map[i64, (u8, i16)] = Map.new();
    m.insert(7, (200, -3));
    let p = m.get(7).unwrap();
    println(f"{p.0} {p.1}");
    let w: Vec[(i32, u8, i64)] = vec![(-1, 200, 3), (5, 7, 9)];
    println(f"{w[1].0} {w[1].1} {w[1].2}");
}
"#,
        &["1 2 3 4", "200 -3", "5 7 9"],
        "b_2026_10_03_23_narrow_tuple_push",
    );
}
