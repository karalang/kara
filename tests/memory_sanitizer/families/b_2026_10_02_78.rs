//! B-2026-10-02-78 -- a `return` that is a block's TAIL hands a by-value
//! `Array` param back from a branch without the frame freeing it.

use super::*;

/// B-2026-10-02-78 — `{ return match a { Some(x) => x, None => d } }`, the
/// `return` with no semicolon. It was never seeded as a frame-escaping site
/// (only the statement spelling and a plain body tail were), so the arm that
/// handed back `d` kept its conditional-move flag armed and the frame's exit
/// freed the elements the caller then read: two invalid reads and two invalid
/// frees at -O0. Covers `match` over another param, `match` over a scalar,
/// `if`, and a `return` that is the tail of an inner `if` block.
#[test]
fn asan_tail_return_branch_hands_back_array_param() {
    assert_clean_asan_run(
        r#"fn g(a: Option[Array[String, 2]], d: Array[String, 2]) -> Array[String, 2] { return match a { Some(x) => x, None => d } }
fn k(n: i64, d: Array[String, 2]) -> Array[String, 2] { return match n { 0 => d, _ => Array[f"x{1}", f"y{2}"] } }
fn f(c: bool, d: Array[String, 2]) -> Array[String, 2] { return if c { d } else { Array[f"x{1}", f"y{2}"] } }
fn h(c: bool, d: Array[String, 2]) -> Array[String, 2] { if c { return if c { d } else { Array[f"x{3}", f"y{4}"] } } Array[f"q{1}", f"r{2}"] }
fn main() {
    let a: Option[Array[String, 2]] = None;
    let r = g(a, Array[f"d{1}", f"e{2}"]); println(f"{r[0]} {r[1]}");
    let b: Option[Array[String, 2]] = Some(Array[f"s{1}", f"t{2}"]);
    let q = g(b, Array[f"d{3}", f"e{4}"]); println(f"{q[0]} {q[1]}");
    let m = k(0, Array[f"d{5}", f"e{6}"]); println(f"{m[0]} {m[1]}");
    let n = k(1, Array[f"d{7}", f"e{8}"]); println(f"{n[0]} {n[1]}");
    let o = f(true, Array[f"d{9}", f"e{10}"]); println(f"{o[0]} {o[1]}");
    let p = h(true, Array[f"d{11}", f"e{12}"]); println(f"{p[0]} {p[1]}");
    let u = h(false, Array[f"d{13}", f"e{14}"]); println(f"{u[0]} {u[1]}")
}
"#,
        &[
            "d1 e2", "s1 t2", "d5 e6", "x1 y2", "d9 e10", "d11 e12", "q1 r2",
        ],
        "B-2026-10-02-78 tail return of a branch hands the array param back",
    );
}

/// B-2026-10-02-78 — the same hand-back with elements that run a user `Drop`
/// body: each body runs once, on the path the value takes.
#[test]
fn asan_tail_return_branch_hands_back_drop_array_param() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mk(n: i64) -> R { R { id: n, s: f"s{n}" } }
fn g(a: Option[Array[R, 2]], d: Array[R, 2]) -> Array[R, 2] { return match a { Some(x) => x, None => d } }
fn h(c: bool, d: Array[R, 2]) -> Array[R, 2] { if c { return if c { d } else { [mk(8), mk(9)] } } [mk(6), mk(7)] }
fn main() {
    let a: Option[Array[R, 2]] = None;
    let r = g(a, [mk(1), mk(2)]); println(f"got {r[0].id} {r[1].id}");
    let b: Option[Array[R, 2]] = Some([mk(3), mk(4)]);
    let q = g(b, [mk(5), mk(10)]); println(f"got {q[0].id} {q[1].id}");
    let t = h(true, [mk(11), mk(12)]); println(f"got {t[0].id}");
    let u = h(false, [mk(13), mk(14)]); println(f"got {u[0].id}");
    println("end")
}
"#,
        &[
            "got 1 2", "d1", "d2", "d5", "d10", "got 3 4", "d3", "d4", "got 11", "d11", "d12",
            "d13", "d14", "got 6", "d6", "d7", "end",
        ],
        "B-2026-10-02-78 Drop elements of a tail-returned array param run once",
    );
}
