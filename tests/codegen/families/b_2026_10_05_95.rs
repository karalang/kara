//! B-2026-10-05-95: a `for` loop over a fresh SortedSet / SortedMap

use super::*;

/// B-2026-10-05-95: a `for` over a SortedSet or SortedMap that is not
/// bound to a name -- a method result such as `a.intersection(b)`, or a call
/// returning one, with or without `.iter()` -- stopped `karac build` at the
/// unlowered-source error, while the same loop over a `Set` or `Map`
/// temporary compiled. Each loop folds its keys into one number in visit
/// order, so a walk in hash or insertion order would show as a different
/// number.
#[test]
fn e2e_for_over_fresh_sorted_set_and_map() {
    let Some(out) = run_program(
        r#"fn ss(xs: ref Vec[i64]) -> SortedSet[i64] {
    let mut s: SortedSet[i64] = SortedSet.new();
    for x in xs { s.insert(x); }
    return s;
}
fn sm(xs: ref Vec[i64]) -> SortedMap[i64, i64] {
    let mut m: SortedMap[i64, i64] = SortedMap.new();
    for x in xs { m.insert(x, x * 10); }
    return m;
}
fn main() {
    let a = [3, 1, 4, 1, 5, 9, 2, 6];
    let b = [9, 2, 6, 5, 3, 5, 8];
    let sa = ss(a);
    let sb = ss(b);
    let mut t = 0;
    for v in sa.intersection(sb) { t = t * 10 + v; }
    println(t);
    t = 0;
    for v in sa.union(sb) { t = t * 10 + v; }
    println(t);
    t = 0;
    for v in ss(b) { t = t * 10 + v; }
    println(t);
    t = 0;
    for (k, v) in sm(a) { t = t * 100 + k + v; }
    println(t);
    t = 0;
    for v in sa.union(sb).iter() { t = t * 10 + v; }
    println(t);
    t = 0;
    for v in ss(b).iter() { t = t * 10 + v; }
    println(t);
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "23569\n12345689\n235689\n11223344556699\n12345689\n235689\n"
    );
}
