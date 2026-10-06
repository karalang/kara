//! B-2026-10-06-82: a non-empty `Set[...]` prefix literal is a `Set`.

use super::*;

/// B-2026-10-06-82: `Set[1, 1, 2]` deduplicates, and `insert` /
/// `contains` / `remove` on a set made by the literal are the `Set` methods
/// (returning `bool`), not `Vec.insert(index, ..)`. `Vec[...]` and
/// `VecDeque[...]` literals keep their sequence semantics.
#[test]
fn test_set_prefix_literal_builds_a_set() {
    let out = run(r#"fn main() {
    let s = Set[1, 1, 2];
    println(f"{s.len()} {s.contains(2)} {s.contains(3)}");
    let mut out: Set[i64] = Set[1, 2];
    let fresh = out.insert(5);
    let again = out.insert(1);
    println(f"{fresh} {again} {out.len()}");
    if out.insert(9) and out.contains(9) {
        println("guarded insert ok");
    }
    println(f"{out.remove(2)} {out.len()}");
    let v = Vec[3, 3, 1];
    let d = VecDeque[4, 4];
    println(f"{v.len()} {d.len()}");
}
"#);
    assert_eq!(
        out,
        "2 true false\ntrue false 3\nguarded insert ok\ntrue 3\n3 2\n"
    );
}
