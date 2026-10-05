//! B-2026-10-05-23: `--interp` answers the type-level drop questions from a
//! memo and an enum index instead of re-scanning the program per value

use super::*;

/// B-2026-10-05-23: a behaviour pin for the memo and the index. Drop-bearing
/// leaves under a recursive enum consumed by a `match` in a loop (the memoized
/// question is asked again for every value), a struct whose field is that
/// enum, a generic enum over a `Drop` type and over `i64`, and both payload
/// shapes. The lines are compared sorted: this pins which bodies run and how
/// often, which is what a wrong memo or index entry would change, and leaves
/// their placement to the drop-order fixtures. Passes with and without the
/// change (measured); it is a cost change.
#[test]
fn interp_drop_memo_and_enum_index_keep_every_body() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum Tree {
    Leaf(R),
    Node(Vec[Tree]),
    Empty,
}
enum Pair[T] {
    One(T),
    Two(T, T),
}
struct Holder { t: Tree, n: i64 }
fn count(t: Tree) -> i64 {
    match t {
        Leaf(r) => { return r.id; }
        Node(kids) => {
            let mut total = 0;
            for k in kids {
                total += count(k);
            }
            return total;
        }
        Empty => { return 0; }
    }
}
fn take(p: Pair[R]) -> i64 {
    match p {
        One(a) => { return a.id; }
        Two(a, b) => { return a.id * 10 + b.id; }
    }
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let t = Node([Leaf(R { id: i }), Node([Leaf(R { id: 10 + i }), Empty])]);
        println(f"count {count(t)}");
        i += 1;
    }
    let h = Holder { t: Leaf(R { id: 7 }), n: 1 };
    println(f"h {h.n}");
    println(f"take {take(One(R { id: 4 }))} {take(Two(R { id: 5 }, R { id: 6 }))}");
    let ps = [Pair.One(1), Pair.Two(2, 3)];
    for p in ps {
        match p {
            One(x) => { println(f"one {x}"); }
            Two(x, y) => { println(f"two {x} {y}"); }
        }
    }
    println("end");
}
"#);
    let mut got: Vec<&str> = out.lines().collect();
    got.sort_unstable();
    assert_eq!(
        got,
        vec![
            "count 10",
            "count 12",
            "count 14",
            "dR0",
            "dR1",
            "dR10",
            "dR11",
            "dR12",
            "dR2",
            "dR4",
            "dR5",
            "dR6",
            "dR7",
            "end",
            "h 1",
            "one 1",
            "take 4 56",
            "two 2 3",
        ]
    );
}
