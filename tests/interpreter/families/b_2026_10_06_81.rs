//! B-2026-10-06-81: a repeat literal or `resize` fill builds
//! independent slots, not `n` handles to one collection.

use super::*;

/// B-2026-10-06-81: every slot of `vec![row; n]`, `[row; n]` and
/// `v.resize(n, row)` is its own copy, so writing one slot leaves the others
/// alone -- for a nested `Vec`, an `Array`, a struct element, a `Map` element
/// and a tuple element. The expected text is what every compiled surface
/// prints.
#[test]
fn test_repeat_literal_slots_are_independent_copies() {
    let out = run(r#"struct P { xs: Vec[i64] }
fn main() {
    let mut dp: Vec[Vec[bool]] = vec![vec![false; 2]; 3];
    dp[2][1] = true;
    println(f"{dp[0][1]} {dp[1][1]} {dp[2][1]}");
    let mut v: Vec[Vec[i64]] = Vec.new();
    v.resize(3, vec![0; 2]);
    v[0][1] = 5;
    println(f"{v[0][1]} {v[1][1]} {v[2][1]}");
    let mut a: Array[Vec[i64], 3] = [vec![1]; 3];
    a[1].push(4);
    println(f"{a[0].len()} {a[1].len()} {a[2].len()}");
    let mut ps = vec![P { xs: vec![] }; 2];
    ps[0].xs.push(1);
    println(f"{ps[0].xs.len()} {ps[1].xs.len()}");
    let mut ms: Vec[Map[i64, i64]] = vec![Map.new(); 2];
    ms[0].insert(1, 1);
    println(f"{ms[0].len()} {ms[1].len()}");
    let mut t: Vec[(Vec[i64], i64)] = vec![(vec![0], 1); 2];
    t[0].0.push(3);
    println(f"{t[0].0.len()} {t[1].0.len()}");
    let mut g: Vec[Vec[i64]] = vec![vec![7]; 0];
    g.resize(2, vec![8]);
    g[1].push(9);
    println(f"{g[0]} {g[1]}");
}
"#);
    assert_eq!(
        out,
        "false false true\n5 0 0\n1 2 1\n1 0\n1 0\n2 1\n[8] [8, 9]\n"
    );
}
