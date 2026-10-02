//! B-2026-10-02-35 — `Vec.swap` is inlined: two loads and two stores of the
//! element type, instead of a call into the runtime that LLVM could not inline.

use super::*;

/// B-2026-10-02-35 — the same program under ASAN: an inline swap of heap-owning
/// elements frees each buffer once, and `i == j` leaves the element intact.
#[test]
fn asan_vec_swap_inline_frees_each_buffer_once() {
    assert_clean_asan_run(
        r#"struct Item { id: i64, tag: String }
impl Drop for Item { fn drop(mut ref self) { println(f"D{self.id}") } }
fn mk(i: i64) -> Item { return Item { id: i, tag: f"tag_{i}_long_enough_to_live_on_the_heap" } }
fn swap_in(a: mut ref Vec[i64], i: i64, j: i64) { a.swap(i, j); }
fn main() {
    let mut v: Vec[i64] = [1, 2, 3, 4, 5];
    swap_in(mut v, 0, 4);
    v.swap(1, 1);
    v.swap(3, 2);
    println(f"{v[0]}{v[1]}{v[2]}{v[3]}{v[4]}");
    let mut s: Vec[String] = ["alpha_heap_string_xxxxxxxxxxxxxxx", "b"];
    s.swap(0, 1);
    println(f"{s[0]} {s[1]}");
    let mut t: Vec[(i64, String)] = [(1, "one_heap_string_xxxxxxxxxxxxxxxxxx"), (2, "two")];
    t.swap(1, 0);
    println(f"{t[0].0}{t[0].1} {t[1].0}{t[1].1}");
    let mut w: Vec[Item] = Vec.new();
    w.push(mk(1));
    w.push(mk(2));
    w.push(mk(3));
    w.swap(0, 2);
    w.swap(1, 1);
    println(f"{w[0].id}{w[1].id}{w[2].id} {w[0].tag}");
    let mut n: Vec[Vec[i64]] = [[1], [2, 3]];
    n.swap(0, 1);
    println(f"{n[0].len()} {n[1][0]}");
    println("end");
}
"#,
        &[
            "52431",
            "b alpha_heap_string_xxxxxxxxxxxxxxx",
            "2two 1one_heap_string_xxxxxxxxxxxxxxxxxx",
            "321 tag_3_long_enough_to_live_on_the_heap",
            "D3",
            "D2",
            "D1",
            "2 1",
            "end",
        ],
        "vec swap inline",
    );
}
