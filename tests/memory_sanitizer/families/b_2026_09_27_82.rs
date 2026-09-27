//! B-2026-09-27-82 — a `Drop` field two or more hops down a named local,
//! moved out on only SOME paths (`if c { xs.push(x.w.r) }`, `if c { let k =
//! x.w.r }`, `if c { return Some(x.w.r) }`).

use super::*;

/// B-2026-09-27-82 — a `Drop` field two or more hops down a named local,
/// moved out inside an `if` (or its `else`), runs its body exactly once on
/// both legs. Compiled code ran it a second time over the freed name on the
/// leg that moved (a call argument or a push), and lost it on the leg that did
/// not (a `let` or a `return`), because the two-hop mask was static while
/// the move was not. Each function runs once with the move and once without.
#[test]
fn asan_two_hop_field_moved_under_an_if_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct W { r: D, s: D }
struct X { w: W, t: D }
struct Y { x: X, u: D }
fn mkx(n: i64) -> X { X { w: W { r: mkd(n), s: mkd(n + 1) }, t: mkd(n + 2) } }
fn keep(d: D) -> D { d }
fn push_if(c: bool) { let x = mkx(10); let mut xs: Vec[D] = Vec.new(); if c { xs.push(x.w.r); } println(f"l{xs.len()}"); }
fn keep_if(c: bool) { let x = mkx(20); if c { let k = keep(x.w.r); println(f"k{k.id}"); } println("r"); }
fn push_else(c: bool) { let x = mkx(30); let mut xs: Vec[D] = Vec.new(); if c { println("a"); } else { xs.push(x.w.s); } println(f"l{xs.len()}"); }
fn three_hops(c: bool) { let y = Y { x: mkx(40), u: mkd(43) }; let mut xs: Vec[D] = Vec.new(); if c { xs.push(y.x.w.r); } println(f"l{xs.len()}"); }
fn let_if(c: bool) { let x = mkx(50); if c { let k = x.w.r; println(f"k{k.id}"); } println("r"); }
fn ret_if(c: bool) -> Option[D] { let x = mkx(60); if c { return Some(x.w.r); } println("n"); None }
fn main() {
    push_if(false); push_if(true);
    keep_if(false); keep_if(true);
    push_else(false); push_else(true);
    three_hops(false); three_hops(true);
    let_if(false); let_if(true);
    match ret_if(false) { Some(d) => println(f"s{d.id}"), None => println("none") }
    match ret_if(true) { Some(d) => println(f"s{d.id}"), None => println("none") }
    println("end")
}
"#,
        &[
            "d12n12", "d11n11", "d10n10", "l0", "d12n12", "d11n11", "l1", "d10n10", "d22n22",
            "d21n21", "d20n20", "r", "k20", "d20n20", "d22n22", "d21n21", "r", "d32n32", "d30n30",
            "l1", "d31n31", "a", "d32n32", "d31n31", "d30n30", "l0", "d43n43", "d42n42", "d41n41",
            "d40n40", "l0", "d43n43", "d42n42", "d41n41", "l1", "d40n40", "d52n52", "d51n51",
            "d50n50", "r", "k50", "d50n50", "d52n52", "d51n51", "r", "d62n62", "d61n61", "d60n60",
            "n", "none", "d62n62", "d61n61", "s60", "d60n60", "end",
        ],
        "asan_two_hop_field_moved_under_an_if_is_freed_once",
        30,
    );
}
