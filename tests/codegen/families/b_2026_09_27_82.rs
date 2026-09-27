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
fn e2e_two_hop_field_moved_under_an_if_runs_its_body_once_on_each_leg() {
    let src = r#"struct D { id: i64, name: String }
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
}"#;
    let want = "d12n12\nd11n11\nd10n10\nl0\nd12n12\nd11n11\nl1\nd10n10\nd22n22\nd21n21\nd20n20\nr\nk20\nd20n20\nd22n22\nd21n21\nr\nd32n32\nd30n30\nl1\nd31n31\na\nd32n32\nd31n31\nd30n30\nl0\nd43n43\nd42n42\nd41n41\nd40n40\nl0\nd43n43\nd42n42\nd41n41\nl1\nd40n40\nd52n52\nd51n51\nd50n50\nr\nk50\nd50n50\nd52n52\nd51n51\nr\nd62n62\nd61n61\nd60n60\nn\nnone\nd62n62\nd61n61\ns60\nd60n60\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-09-27-101 — the same two-hop leaf moved out as a BRANCH ARM'S TAIL
/// VALUE (`let y = if c { x.w.r } else { .. }`, a `match` arm, the `else`
/// leg, both legs, three hops, and as a call argument) runs its body once on
/// each leg. The interpreter ran it a second time from the root's walk and
/// compiled code read it again as a husk, because the arm-tail recorders on
/// both backends took one hop only.
#[test]
fn e2e_two_hop_field_as_an_arm_tail_value_runs_its_body_once_on_each_leg() {
    let src = r#"struct D { id: i64, name: String }
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
}"#;
    let want = "d12n12\nd11n11\nd10n10\ny9\nd9n9\nd12n12\nd11n11\ny10\nd10n10\nd22n22\nd21n21\nd20n20\ny9\nd9n9\nd22n22\nd21n21\ny20\nd20n20\nd32n32\nd30n30\ny31\nd31n31\nd32n32\nd31n31\ny30\nd30n30\nd42n42\nd41n41\ny40\nd40n40\nd42n42\nd41n41\nd40n40\ny9\nd9n9\nd53n53\nd52n52\nd51n51\nd50n50\ny9\nd9n9\nd53n53\nd52n52\nd51n51\ny50\nd50n50\nd62n62\nd61n61\nd60n60\nl1\nd9n9\nd62n62\nd61n61\nl1\nd60n60\ns9\nd9n9\nd72n72\nd71n71\nd70n70\nr\ns70\nd70n70\nd72n72\nd71n71\nr\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
