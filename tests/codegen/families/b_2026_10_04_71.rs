//! B-2026-10-04-71: a nested store runs each subscript once compiled, the
//! count `--interp` now matches

use super::*;

/// B-2026-10-04-71: the compiled half of the interpreter fixtures of the same
/// name. Compiled code already ran each subscript of these stores once, after
/// the value and in source order, and a compound store's once per half (read,
/// then store after the right-hand side); this pins the counts both backends
/// now agree on, so a regression on either side shows as a differential.
#[test]
fn e2e_nested_store_runs_each_subscript_once() {
    let Some(out) = run_program(
        r#"struct P { n: i64, s: String }
struct H { xs: Vec[Vec[i64]], t: (i64, Vec[i64]) }
fn idx(k: i64) -> i64 { println(f"idx{k}"); return k }
fn val(k: i64) -> i64 { println(f"val{k}"); return k }
fn key(k: String) -> String { println(f"key{k}"); return k }
fn main() {
    let mut w: Vec[Vec[i64]] = [[1, 2], [3, 4]];
    w[idx(0)][idx(1)] = val(5);
    let mut u: Vec[Vec[P]] = [[P { n: 1, s: "a" }, P { n: 1, s: "a" }], [P { n: 1, s: "a" }, P { n: 1, s: "a" }]];
    u[idx(1)][idx(0)].n = val(6);
    w[idx(1)][idx(0)] += val(2);
    println(f"{w[0][1]} {u[1][0].n} {w[1][0]}");
    let mut mv: Vec[Map[String, i64]] = [Map.new()];
    mv[idx(0)][key("a")] = 1;
    let mut h = H { xs: [[1, 2], [3, 4]], t: (0, [5, 6]) };
    h.xs[idx(1)][idx(0)] = 9;
    h.t.1[idx(1)] = 8;
    let mut hs: Vec[H] = [H { xs: [[1]], t: (0, [5]) }];
    hs[idx(0)].xs[idx(0)][idx(0)] = 7;
    println(f"{mv[0].len()} {h.xs[1][0]} {h.t.1[1]} {hs[0].xs[0][0]}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "val5\nidx0\nidx1\nval6\nidx1\nidx0\nidx1\nidx0\nval2\nidx1\nidx0\n5 6 5\n\
         idx0\nkeya\nidx1\nidx0\nidx1\nidx0\nidx0\nidx0\n1 9 8 7\n"
    );
}
