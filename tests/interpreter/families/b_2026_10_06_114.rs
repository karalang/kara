//! B-2026-10-06-114: a place rooted at a `Map` entry chain
//! (`m.entry(k).or_insert(d)…`) behaves like its bound spelling
//! `let r = m.entry(k).or_insert(d); r…` under the interpreter.

use super::*;

/// B-2026-10-06-114: field, tuple-element and index reads through the chain
/// used to panic on the raw slot ref; a by-value receiver (`push_str`, a
/// `mut ref self` method) lost its write; and `*… += 1` ran the chain, and
/// with it `noisy()`, twice.
#[test]
fn test_entry_chain_places_read_and_write_the_slot() {
    let out = run(r#"struct Bag { items: Vec[i64], n: i64, name: String }
impl Bag {
    fn new() -> Bag { Bag { items: Vec.new(), n: 0, name: String.new() } }
    fn add(mut ref self, x: i64) { self.n += x; }
}
fn noisy() -> i64 { println("noisy"); 10 }
fn main() {
    let mut m: Map[i64, Bag] = Map.new();
    m.entry(1).or_insert(Bag.new()).items.push(5);
    m.entry(1).or_insert(Bag.new()).n += 3;
    m.entry(1).or_insert(Bag.new()).n = m[1].n * 2;
    m.entry(1).or_insert(Bag.new()).name.push_str("x");
    m.entry(1).or_insert(Bag.new()).add(4);
    m.entry(2).or_insert_with(|| Bag.new()).items.push(9);
    println(f"read {m.entry(1).or_insert(Bag.new()).n} {m.entry(1).or_insert(Bag.new()).items.len()}");
    println(f"{m[1].items} {m[1].n} [{m[1].name}] {m[2].items}");
    let mut s: Map[i64, String] = Map.new();
    s.entry(1).or_insert(String.from("a")).push_str("b");
    println(f"{s[1]}");
    let mut c: Map[i64, i64] = Map.new();
    *c.entry(5).or_insert(noisy()) += 2;
    *c.entry(5).or_insert(noisy()) = c[5] * 3;
    println(f"{c[5]}");
    let mut t: Map[i64, (i64, i64)] = Map.new();
    t.entry(1).or_insert((1, 2)).0 += 5;
    let mut v: Map[i64, Vec[i64]] = Map.new();
    v.entry(1).or_insert(vec![4, 8])[1] += 1;
    println(f"{t[1].0} {t.entry(1).or_insert((0, 0)).1} {v.entry(1).or_insert(Vec.new())[1]}");
}
"#);
    assert_eq!(
        out,
        "read 10 1\n[5] 10 [x] [9]\nab\nnoisy\nnoisy\n36\n6 2 9\n"
    );
}
