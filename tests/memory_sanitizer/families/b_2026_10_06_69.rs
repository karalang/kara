//! B-2026-10-06-69 -- a `Set` / `SortedSet` copied by the generic map clone
//! helper got an 8-byte value slot, so a later `remove` wrote 8 bytes into a
//! one-byte stack slot (a stack-buffer-overflow under ASAN).

use super::*;

#[test]
fn asan_set_clone_remove_writes_no_value_bytes() {
    assert_clean_asan_run(
        r#"struct Holder { m: Map[i64, i64], s: Set[i64] }
struct SHolder { m: SortedMap[i64, i64], s: SortedSet[i64] }
fn hashed() {
    let a: Map[i64, i64] = Map.new();
    let mut s: Set[i64] = Set.new();
    s.insert(5);
    let t = s.clone();
    let h = Holder { m: a.clone(), s: t.clone() };
    let mut h2 = Holder { m: h.m.clone(), s: h.s.clone() };
    h2.s.remove(5);
    println(f"a:{h2.s.len()} {h.s.len()}");
}
fn sorted() {
    let a: SortedMap[i64, i64] = SortedMap.new();
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(5);
    let t = s.clone();
    let h = SHolder { m: a.clone(), s: t.clone() };
    let mut h2 = SHolder { m: h.m.clone(), s: h.s.clone() };
    h2.s.remove(5);
    println(f"b:{h2.s.len()} {h.s.len()}");
}
fn strings() {
    let mut s: Set[String] = Set.new();
    s.insert(f"k{1}");
    let mut c = s.clone();
    c.remove(f"k{1}");
    println(f"c:{c.len()} {s.len()}");
}
fn main() { hashed(); sorted(); strings(); }
"#,
        &["a:0 1", "b:0 1", "c:0 1"],
        "asan_set_clone_remove_writes_no_value_bytes",
    );
}
