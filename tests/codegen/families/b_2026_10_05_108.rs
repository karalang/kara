//! B-2026-10-05-108: `flat_map` with a destructuring param or a mapped inner

use super::*;

/// B-2026-10-05-108: `sm.iter().flat_map(|(k, n)| (0..n).map(|_| k))
/// .collect()` and `v.iter().flat_map(|x| (0..x).map(|_| x)).collect()`
/// stopped `karac build` at "no handler for method 'collect'", and the same
/// flat_map over a Map or SortedMap in a `for` loop stopped at "for-loop over
/// the `.flat_map()` iterator adaptor is not yet lowered", while `--interp`
/// ran every one. Each form here now builds and prints what `--interp` does,
/// including a filter before the flat_map, a wildcard in the tuple, a
/// downstream `map`, the `sum` / `count` terminals, and a `for` loop whose
/// param is a flat or nested tuple (the bail contract's old
/// `flat_map (destructuring closure param)` case).
#[test]
fn e2e_flat_map_with_tuple_param_or_mapped_inner() {
    let Some(out) = run_program(
        r#"fn main() {
    let v = [3, 1, 2];
    let pr = [(1, 2), (5, 0), (7, 3)];
    let mut sm: SortedMap[i64, i64] = SortedMap.new();
    sm.insert(1, 2);
    sm.insert(4, 1);
    let mut m: Map[i64, i64] = Map.new();
    m.insert(5, 2);
    let a: Vec[i64] = v.iter().flat_map(|x| (0..x).map(|_| x)).collect();
    let b: Vec[i64] = sm.iter().flat_map(|(k, n)| (0..n).map(|_| k)).collect();
    let c: Vec[i64] = m.iter().flat_map(|(k, n)| Vec.filled(n, k).into_iter()).collect();
    let d: Vec[i64] = sm.iter().filter(|(k, _)| k > 1).flat_map(|(k, n)| (0..n).map(|_| k * 10)).collect();
    let e: Vec[i64] = pr.iter().flat_map(|(_, b)| [b, b].iter()).collect();
    let f: Vec[i64] = pr.iter().flat_map(|(a, n)| (0..n).map(|i| a + i)).map(|y| y * 2).collect();
    println(f"{a} {b} {c} {d} {e} {f}");
    let mut g: Vec[i64] = Vec.new();
    for x in sm.iter().flat_map(|(k, n)| (0..n).map(|_| k)) {
        g.push(x);
    }
    let s = sm.iter().flat_map(|(k, n)| (0..n).map(|_| k)).sum();
    let n = m.iter().flat_map(|(k, n)| (0..n).map(|_| k)).count();
    println(f"{g} {s} {n}");
    let ps: Vec[(i64, i64)] = [(1, 2), (3, 4)];
    let vv: Vec[Vec[i64]] = [[1]];
    let mut h = 0;
    for y in ps.iter().flat_map(|(a, b)| vv[0].iter()) {
        h = h + y;
    }
    let qs: Vec[(i64, (i64, i64))] = [(1, (2, 5))];
    let mut t = 0;
    for y in qs.iter().flat_map(|(a, (b, c))| [a, b, c].iter()) {
        t = t + y;
    }
    println(f"{h} {t}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "[3, 3, 3, 1, 2, 2] [1, 1, 4] [5, 5] [40] [2, 2, 0, 0, 3, 3] [2, 4, 14, 16, 18]\n[1, 1, 4] 6 2\n2 8\n"
    );
}
