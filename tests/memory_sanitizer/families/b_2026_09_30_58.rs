//! B-2026-09-30-58 -- a branching argument whose tails are collection
//! literals, or mix a block-local binding with a call, frees its `Vec` buffer.

use super::*;

/// B-2026-09-30-58 — `vl(if c { [1] } else { [2, 3] })`, its `match`
/// spelling and `vl(if c { let t = mk(1); t } else { mk(2) })` each lost one
/// block per call on every compiled surface: a collection-literal tail was
/// not counted as minting a fresh buffer, and an `if` THEN arm's block-local
/// binding tail was classified by its bare identifier. Covers both arms
/// taken, a closure callee, a loop, a method receiver, a `for` iterable,
/// scalar-arithmetic and negated items, `[v; n]` / `Vec[..]` spellings, and
/// `String` items that are literals or fresh calls.
#[test]
fn asan_branch_literal_tail_arg_frees_buffer() {
    assert_clean_asan_run(
        r#"fn mk(n: i64) -> Vec[i64] { return [n, n + 1] }
fn vl(x: Vec[i64]) -> i64 { return x[0] + x.len() }
fn ms(n: i64) -> String { return f"ssssssssssssssssssssssssssssss{n}" }
fn vs(x: Vec[String]) -> i64 { return x.len() }
fn run(c: bool) {
    let n = 7;
    let a = vl(if c { [1] } else { [2, 3] });
    let b = vl(match c { true => [4], false => [n, 5] });
    let d = vl(if c { let t = mk(10); t } else { mk(20) });
    let f = |x: Vec[i64]| x[0];
    let e = f(if c { [30] } else { Vec[31, 32] });
    let g = vl(if c { [1; 4] } else { [-2] });
    let h = (if c { [n * 2] } else { [8, 9] }).len();
    let mut s = 0;
    for x in (if c { [1] } else { [2, 3] }) { s = s + x; }
    let u = vs(if c { ["x".to_string()] } else { ["a".to_string(), ms(2)] });
    let v = vs(match c { true => [ms(4)], false => [] });
    println(f"{a} {b} {d} {e} {g} {h} {s} {u} {v}");
}
fn main() {
    run(true);
    run(false);
    let mut i = 0;
    let mut t = 0;
    while i < 3 {
        t = t + vl(if i % 2 == 0 { [i] } else { let q = mk(i); q });
        i = i + 1;
    }
    println(f"t{t}");
}
"#,
        &["2 5 12 30 5 1 1 1 1", "4 9 22 31 -1 2 5 2 0", "t7"],
        "branch_literal_tail_arg",
    );
}
