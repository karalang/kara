//! B-2026-09-27-82 — a `Drop` field two or more hops down a named local,
//! moved out on only SOME paths (`if c { xs.push(x.w.r) }`, `if c { let k =
//! x.w.r }`, `if c { return Some(x.w.r) }`), and B-2026-09-27-101: the same
//! leaf moved out as a branch arm's tail value.

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

/// B-2026-09-27-101 — the same two-hop leaf moved out as a BRANCH ARM'S TAIL
/// VALUE (`let y = if c { x.w.r } else { .. }`, a `match` arm, the `else`
/// leg, both legs, three hops, and as a call argument) runs its body once on
/// each leg. The interpreter ran it a second time from the root's walk and
/// compiled code read it again as a husk, because the arm-tail recorders on
/// both backends took one hop only.
#[test]
fn asan_two_hop_field_as_an_arm_tail_value_is_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"d{self.id}{self.name}") } }
fn mkd(n: i64) -> D { D { id: n, name: f"n{n}" } }
struct W { r: D, s: D }
struct X { w: W, t: D }
struct Y { x: X, u: D }
fn mkx(n: i64) -> X { X { w: W { r: mkd(n), s: mkd(n + 1) }, t: mkd(n + 2) } }
fn show(d: D) { println(f"s{d.id}"); }
fn let_if(c: bool) { let x = mkx(10); let y = if c { x.w.r } else { mkd(9) }; println(f"y{y.id}"); }
fn let_match(c: bool) { let x = mkx(20); let y = match c { true => x.w.r, false => mkd(9) }; println(f"y{y.id}"); }
fn both_legs(c: bool) { let x = mkx(30); let y = if c { x.w.r } else { x.w.s }; println(f"y{y.id}"); }
fn else_leg(c: bool) { let x = mkx(40); let y = if c { mkd(9) } else { x.w.r }; println(f"y{y.id}"); }
fn three_hops(c: bool) { let y0 = Y { x: mkx(50), u: mkd(53) }; let y = if c { y0.x.w.r } else { mkd(9) }; println(f"y{y.id}"); }
fn push_arg(c: bool) { let x = mkx(60); let mut xs: Vec[D] = Vec.new(); xs.push(if c { x.w.r } else { mkd(9) }); println(f"l{xs.len()}"); }
fn call_arg(c: bool) { let x = mkx(70); show(if c { x.w.r } else { mkd(9) }); println("r"); }
fn main() {
    let_if(false); let_if(true);
    let_match(false); let_match(true);
    both_legs(false); both_legs(true);
    else_leg(false); else_leg(true);
    three_hops(false); three_hops(true);
    push_arg(false); push_arg(true);
    call_arg(false); call_arg(true);
    println("end")
}
"#,
        &[
            "d12n12", "d11n11", "d10n10", "y9", "d9n9", "d12n12", "d11n11", "y10", "d10n10",
            "d22n22", "d21n21", "d20n20", "y9", "d9n9", "d22n22", "d21n21", "y20", "d20n20",
            "d32n32", "d30n30", "y31", "d31n31", "d32n32", "d31n31", "y30", "d30n30", "d42n42",
            "d41n41", "y40", "d40n40", "d42n42", "d41n41", "d40n40", "y9", "d9n9", "d53n53",
            "d52n52", "d51n51", "d50n50", "y9", "d9n9", "d53n53", "d52n52", "d51n51", "y50",
            "d50n50", "d62n62", "d61n61", "d60n60", "l1", "d9n9", "d62n62", "d61n61", "l1",
            "d60n60", "s9", "d9n9", "d72n72", "d71n71", "d70n70", "r", "s70", "d70n70", "d72n72",
            "d71n71", "r", "end",
        ],
        "asan_two_hop_field_as_an_arm_tail_value_is_freed_once",
        40,
    );
}
