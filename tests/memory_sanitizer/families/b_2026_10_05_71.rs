//! B-2026-10-05-71 — a tuple destructured straight out of a branch expression.

use super::*;

/// A tuple destructured straight out of a `match`, `if` or block whose every
/// value leaf is a tuple literal took the place-source path, whose "source" is
/// a merged temporary nothing frees: compiled, every leaf leaked and a leaf's
/// `Drop` body never ran. Covers `match` / `if` / block RHS, `String` and
/// `Drop` leaves, wildcard leaves, a named local moved into one arm (taken and
/// not taken), a nested tuple leaf, a loop, a leaf returned, handed to a
/// by-value callee, and rebound.
#[test]
fn asan_tuple_destructured_from_a_branch_owns_its_leaves() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn eat(r: R) { println(f"eat{r.id}") }
fn strs(x: ref String) -> String { let (v, n) = match 1 { 1 => (x.clone(), 1), _ => (x.clone(), 2) }; v.clone() }
fn iff(x: ref String) { let (v, n) = if x.len() > 1 { (x.clone(), 1) } else { (x.clone(), 2) }; println(f"{v} {n}") }
fn mat() { let (r, n) = match 1 { 1 => (mk(1), 1), _ => (mk(2), 2) }; println(f"{r.id} {n}") }
fn blk() { let (r, n) = { let k = 5; (mk(k), 1) }; println(f"{r.id} {n}") }
fn two() { let (r, s) = match 1 { 1 => (mk(6), mk(7)), _ => (mk(8), mk(9)) }; println(f"{r.id} {s.id}") }
fn wild() { let (r, _) = match 1 { 1 => (mk(10), mk(11)), _ => (mk(12), mk(13)) }; println(f"{r.id}"); let (_, n) = match 1 { 1 => (mk(14), 1), _ => (mk(15), 2) }; println(f"{n}") }
fn moved(k: i64) { let w = mk(16); let (r, n) = match k { 1 => (w, 1), _ => (mk(17), 2) }; println(f"{r.id} {n}") }
fn nested() { let (t, m) = match 1 { 1 => ((mk(41), 5), 1), _ => ((mk(42), 6), 2) }; println(f"t{t.0.id} {m}") }
fn looped() { let mut i = 0; while i < 3 { let (r, k) = if i > 0 { (mk(50 + i), i) } else { (mk(60), 0) }; println(f"l{r.id} {k}"); i = i + 1; } }
fn ret(c: bool) -> R { let (r, n) = match c { true => (mk(30), 1), false => (mk(31), 2) }; r }
fn handed() { let (r2, _) = match 1 { 1 => (mk(43), mk(44)), _ => (mk(45), mk(46)) }; eat(r2); println("after-eat") }
fn rebound() { let (r3a, _) = match 1 { 1 => (mk(47), 0), _ => (mk(48), 0) }; let mut r3 = r3a; r3 = mk(49); println(f"r3 {r3.id}") }
fn main() {
    let x = f"cd{1}";
    println(strs(x)); iff(x); mat(); blk(); two(); wild(); moved(1); moved(2); nested(); looped();
    let q = ret(true); println(f"q{q.id}");
    handed(); rebound();
    println("end")
}
"#,
        &[
            "cd1",
            "cd1 1",
            "1 1",
            "drop1",
            "5 1",
            "drop5",
            "6 7",
            "drop7",
            "drop6",
            "drop11",
            "10",
            "drop10",
            "drop14",
            "1",
            "16 1",
            "drop16",
            "drop16",
            "17 2",
            "drop17",
            "t41 1",
            "drop41",
            "l60 0",
            "drop60",
            "l51 1",
            "drop51",
            "l52 2",
            "drop52",
            "q30",
            "drop30",
            "drop44",
            "eat43",
            "drop43",
            "after-eat",
            "drop47",
            "r3 49",
            "drop49",
            "end",
        ],
        "tuple_destructured_from_a_branch_owns_its_leaves",
    );
}
